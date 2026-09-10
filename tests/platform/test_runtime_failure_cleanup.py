#!/usr/bin/env python3
"""Prove injected runtime failure retains evidence and cleans owned PIDs."""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import subprocess
import sys


def same_process(item: dict) -> bool:
    try:
        return os.getpgid(item["pid"]) == item["process_group"]
    except ProcessLookupError:
        return False


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("harness", type=pathlib.Path)
    parser.add_argument("binary", type=pathlib.Path)
    parser.add_argument("--evidence", type=pathlib.Path, required=True)
    parser.add_argument("--expect-piped-stdin", choices=("supported", "unsupported"), required=True)
    args = parser.parse_args()
    result = subprocess.run(
        [
            sys.executable, str(args.harness), str(args.binary),
            "--expect-piped-stdin", args.expect_piped_stdin,
            "--evidence", str(args.evidence),
            "--inject-failure-after-command-spawn",
        ],
        check=False,
    )
    assert result.returncode != 0, "injected runtime failure unexpectedly passed"
    evidence = json.loads(args.evidence.read_text())
    assert evidence["accepted"] is False
    assert evidence["failed_phase"] == "orderly_command_tree_cleanup"
    assert evidence["failure"]["message"] == "injected failure after owned command spawn"
    assert evidence["binary_sha256"]
    assert evidence["transcripts"]["command_capture"]["sha256"]
    assert evidence["owned_processes"]
    assert evidence["cleanup"]["survivors"] == []
    assert evidence["cleanup"]["error"] is None
    assert not [item for item in evidence["owned_processes"] if same_process(item)]
    print("injected runtime failure retained evidence and cleaned every owned identity")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
