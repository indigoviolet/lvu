#!/usr/bin/env python3
"""Run a platform compile probe and write machine-readable evidence.

The Windows mode characterizes known blockers; it is not a support test. Each
target package is classified independently from Cargo JSON diagnostics, and
the exact first dependency-error multiset, package and path must all match.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import platform
import re
import subprocess
import sys
from collections import Counter
from dataclasses import dataclass
from datetime import datetime, timezone


@dataclass(frozen=True)
class WindowsBlocker:
    package: str
    diagnostic_package: str
    path: str
    expected_errors: tuple[tuple[str, str], ...]


ANSI = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
WINDOWS_JOURNAL_ERRORS = (
    ("E0433", "cannot find `unix` in `os`"),
    ("E0433", "cannot find `fd` in `os`"),
    ("E0433", "cannot find `fd` in `os`"),
    ("E0433", "cannot find `fd` in `os`"),
    ("E0433", "cannot find `fd` in `os`"),
    ("E0599", "no method named `dev` found for struct `Metadata` in the current scope"),
    ("E0599", "no method named `ino` found for struct `Metadata` in the current scope"),
)
WINDOWS_BLOCKERS = tuple(
    WindowsBlocker(
        package,
        "lvu-core",
        "crates/lvu-core/src/journal.rs",
        WINDOWS_JOURNAL_ERRORS,
    )
    for package in ("lvu-query", "lvu-command-enrich")
)


def command_output(command: list[str]) -> str:
    result = subprocess.run(command, text=True, capture_output=True, check=False)
    return (result.stdout or result.stderr).strip()


def parse_cargo_output(stdout: bytes) -> tuple[list[dict], list[str], list[dict], str]:
    errors: list[dict] = []
    malformed: list[str] = []
    build_finished: list[dict] = []
    last_reason = ""
    for raw_line in stdout.splitlines():
        if not raw_line.strip():
            continue
        try:
            message = json.loads(raw_line)
        except (UnicodeDecodeError, json.JSONDecodeError):
            malformed.append(raw_line.decode("utf-8", "replace"))
            continue
        last_reason = str(message.get("reason", ""))
        if last_reason == "build-finished":
            build_finished.append(message)
        if message.get("reason") != "compiler-message":
            continue
        diagnostic = message.get("message") or {}
        if diagnostic.get("level") == "error":
            errors.append(
                {**diagnostic, "cargo_package_id": message.get("package_id", "")}
            )
    return errors, malformed, build_finished, last_reason


def primary_paths(diagnostic: dict) -> set[str]:
    return {
        str(span.get("file_name", "")).replace("\\", "/")
        for span in diagnostic.get("spans", [])
        if span.get("is_primary")
    }


def diagnostic_signature(diagnostic: dict) -> tuple[str, str]:
    code = (diagnostic.get("code") or {}).get("code") or ""
    message = ANSI.sub("", str(diagnostic.get("message", "")))
    return code, message


def diagnostic_package(diagnostic: dict) -> str:
    package_id = str(diagnostic.get("cargo_package_id", "")).replace("\\", "/")
    match = re.search(r"/([^/#]+)#(?:[^#]+)$", package_id)
    return match.group(1) if match else ""


def classify_windows_run(
    blocker: WindowsBlocker, returncode: int, stdout: bytes, stderr: bytes
) -> tuple[bool, str, list[dict]]:
    errors, malformed_stdout, build_finished, last_reason = parse_cargo_output(stdout)
    signatures = [
        {
            "code": diagnostic_signature(error)[0],
            "message": diagnostic_signature(error)[1],
            "primary_paths": sorted(primary_paths(error)),
            "package": diagnostic_package(error),
        }
        for error in errors
    ]
    actual_errors = Counter(diagnostic_signature(error) for error in errors)
    expected_errors = Counter(blocker.expected_errors)
    stderr_text = ANSI.sub("", stderr.decode("utf-8", "replace"))
    unexpected_cargo_errors = [
        line.strip()
        for line in stderr_text.splitlines()
        if line.lstrip().startswith("error:")
        and not (
            line.lstrip().startswith(
                f"error: could not compile `{blocker.diagnostic_package}`"
            )
            and "previous error" in line
        )
    ]
    if returncode != 101:
        return False, f"Cargo exited {returncode}; expected compile-error exit 101", signatures
    if malformed_stdout:
        return False, "Cargo stdout contained non-JSON output", signatures
    if (
        len(build_finished) != 1
        or build_finished[0].get("success") is not False
        or last_reason != "build-finished"
    ):
        return False, "Cargo did not emit one terminal build-finished:false record", signatures
    if actual_errors != expected_errors:
        return False, "compiler errors did not exactly match the known dependency blocker", signatures
    if any(primary_paths(error) != {blocker.path} for error in errors):
        return False, "a compiler error came from an unexpected source path", signatures
    if any(diagnostic_package(error) != blocker.diagnostic_package for error in errors):
        return False, "a compiler error came from an unexpected package", signatures
    if unexpected_cargo_errors:
        return False, "Cargo reported an unrelated setup/tool failure", signatures
    return True, "known Windows dependency blocker reproduced exactly", signatures


def compose_log(commands: list[list[str]], runs: list[subprocess.CompletedProcess]) -> bytes:
    chunks = []
    for command, run in zip(commands, runs, strict=True):
        chunks.extend(
            (
                ("+ " + " ".join(command) + "\n").encode(),
                b"--- cargo stdout ---\n",
                run.stdout,
                b"\n--- cargo stderr ---\n",
                run.stderr,
                b"\n",
            )
        )
    return b"".join(chunks)


def write_log(path: pathlib.Path, content: bytes) -> str:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(content)
    written = path.read_bytes()
    if written != content:
        raise OSError("compile log bytes changed while writing")
    return hashlib.sha256(written).hexdigest()


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--target", required=True)
    parser.add_argument(
        "--expect", choices=("success", "windows-known-blockers"), required=True
    )
    parser.add_argument("--evidence", type=pathlib.Path, required=True)
    parser.add_argument("--log", type=pathlib.Path, required=True)
    args = parser.parse_args()

    if args.expect == "success":
        commands = [[
            "cargo", "check", "--workspace", "--all-targets", "--locked",
            "--target", args.target, "--message-format=json",
        ]]
        blockers: list[WindowsBlocker | None] = [None]
    else:
        commands = [
            [
                "cargo", "check", "-p", blocker.package, "--all-targets",
                "--locked", "--target", args.target, "--message-format=json",
            ]
            for blocker in WINDOWS_BLOCKERS
        ]
        blockers = list(WINDOWS_BLOCKERS)

    runs = [
        subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        for command in commands
    ]
    raw_log = compose_log(commands, runs)
    sys.stdout.buffer.write(raw_log)
    log_sha256 = write_log(args.log, raw_log)

    classifications = []
    if args.expect == "success":
        accepted = runs[0].returncode == 0
        reason = "workspace all-target compile succeeded" if accepted else "compile failed"
    else:
        for blocker, run in zip(blockers, runs, strict=True):
            assert blocker is not None
            accepted_run, run_reason, diagnostics = classify_windows_run(
                blocker, run.returncode, run.stdout, run.stderr
            )
            classifications.append(
                {
                    "target_package": blocker.package,
                    "diagnostic_package": blocker.diagnostic_package,
                    "path": blocker.path,
                    "expected_errors": [
                        {"code": code, "message": message}
                        for code, message in blocker.expected_errors
                    ],
                    "accepted": accepted_run,
                    "reason": run_reason,
                    "compiler_errors": diagnostics,
                }
            )
        accepted = all(item["accepted"] for item in classifications)
        reason = (
            "each target reproduced the exact known Windows dependency blocker"
            if accepted
            else "at least one target did not match its exact dependency blocker"
        )

    evidence = {
        "schema_version": 1,
        "kind": "platform_compile_probe",
        "recorded_at_utc": datetime.now(timezone.utc).isoformat(),
        "target": args.target,
        "expectation": args.expect,
        "accepted": accepted,
        "reason": reason,
        "cargo_exits": [run.returncode for run in runs],
        "classifications": classifications,
        "commands": commands,
        "host": platform.platform(),
        "machine": platform.machine(),
        "rustc": command_output(["rustc", "--version", "--verbose"]),
        "source_sha": os.environ.get("GITHUB_SHA") or command_output(["git", "rev-parse", "HEAD"]),
        "log": str(args.log),
        "log_sha256": log_sha256,
    }
    args.evidence.parent.mkdir(parents=True, exist_ok=True)
    args.evidence.write_text(json.dumps(evidence, indent=2) + "\n", encoding="utf-8")
    if not accepted:
        print(f"platform compile probe rejected: {reason}", file=sys.stderr)
        return 1
    print(f"platform compile probe accepted: {reason}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
