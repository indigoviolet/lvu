#!/usr/bin/env python3
"""Focused synthetic regressions for platform_compile_probe.py."""

from __future__ import annotations

import hashlib
import json
import pathlib
import tempfile
import unittest

from platform_compile_probe import WINDOWS_BLOCKERS, classify_windows_run, write_log


def diagnostic(blocker, code, message, *, path=None, package=None):
    package_name = package or blocker.diagnostic_package
    payload = {
        "reason": "compiler-message",
        "package_id": f"path+file:///D:/a/lvu/lvu/crates/{package_name}#0.1.0",
        "message": {
            "level": "error",
            "code": {"code": code},
            "message": message,
            "spans": [{"file_name": path or blocker.path, "is_primary": True}],
        },
    }
    return json.dumps(payload).encode() + b"\n"


def cargo_finished(success=False):
    return json.dumps({"reason": "build-finished", "success": success}).encode() + b"\n"


def exact_diagnostics(blocker):
    chunks = (
        diagnostic(blocker, code, message)
        for code, message in blocker.expected_errors
    )
    return b"".join(chunks)


def exact_stdout(blocker):
    return exact_diagnostics(blocker) + cargo_finished()


class CompileProbeTests(unittest.TestCase):
    def test_exact_dependency_blocker_passes_for_each_target(self):
        for blocker in WINDOWS_BLOCKERS:
            accepted, _, _ = classify_windows_run(
                blocker, 101, exact_stdout(blocker), b""
            )
            self.assertTrue(accepted)

    def test_exact_first_and_unrelated_second_failure_rejects(self):
        first, second = WINDOWS_BLOCKERS
        first_ok, _, _ = classify_windows_run(first, 101, exact_stdout(first), b"")
        unrelated = diagnostic(second, "E0308", "mismatched types") + cargo_finished()
        second_ok, _, _ = classify_windows_run(second, 101, unrelated, b"")
        self.assertTrue(first_ok)
        self.assertFalse(second_ok)

    def test_extra_compiler_error_rejects(self):
        blocker = WINDOWS_BLOCKERS[0]
        extra = diagnostic(blocker, "E0308", "mismatched types")
        accepted, _, _ = classify_windows_run(
            blocker, 101, exact_diagnostics(blocker) + extra + cargo_finished(), b""
        )
        self.assertFalse(accepted)

    def test_missing_dependency_error_rejects(self):
        blocker = WINDOWS_BLOCKERS[0]
        partial = blocker.expected_errors[:-1]
        stdout = b"".join(diagnostic(blocker, *item) for item in partial) + cargo_finished()
        accepted, _, _ = classify_windows_run(blocker, 101, stdout, b"")
        self.assertFalse(accepted)

    def test_signal_exit_with_exact_complete_stream_rejects(self):
        blocker = WINDOWS_BLOCKERS[0]
        accepted, _, _ = classify_windows_run(
            blocker, -9, exact_stdout(blocker), b""
        )
        self.assertFalse(accepted)

    def test_ordinary_exit_with_exact_complete_stream_rejects(self):
        blocker = WINDOWS_BLOCKERS[0]
        accepted, _, _ = classify_windows_run(
            blocker, 1, exact_stdout(blocker), b""
        )
        self.assertFalse(accepted)

    def test_missing_terminal_record_rejects(self):
        blocker = WINDOWS_BLOCKERS[0]
        accepted, _, _ = classify_windows_run(
            blocker, 101, exact_diagnostics(blocker), b""
        )
        self.assertFalse(accepted)

    def test_success_terminal_record_rejects(self):
        blocker = WINDOWS_BLOCKERS[0]
        stdout = exact_diagnostics(blocker) + cargo_finished(success=True)
        accepted, _, _ = classify_windows_run(blocker, 101, stdout, b"")
        self.assertFalse(accepted)

    def test_duplicate_terminal_records_reject(self):
        blocker = WINDOWS_BLOCKERS[0]
        stdout = exact_stdout(blocker) + cargo_finished()
        accepted, _, _ = classify_windows_run(blocker, 101, stdout, b"")
        self.assertFalse(accepted)

    def test_record_after_terminal_rejects(self):
        blocker = WINDOWS_BLOCKERS[0]
        trailing = json.dumps({"reason": "compiler-artifact"}).encode() + b"\n"
        accepted, _, _ = classify_windows_run(
            blocker, 101, exact_stdout(blocker) + trailing, b""
        )
        self.assertFalse(accepted)

    def test_setup_failure_alongside_exact_errors_rejects(self):
        blocker = WINDOWS_BLOCKERS[0]
        accepted, _, _ = classify_windows_run(
            blocker,
            101,
            exact_stdout(blocker),
            b"error: linker or toolchain setup failed\n",
        )
        self.assertFalse(accepted)

    def test_crlf_log_hash_authenticates_written_bytes(self):
        content = b"cargo line one\r\ncargo line two\r\n"
        with tempfile.TemporaryDirectory(prefix="lvu-platform-probe-") as temporary:
            path = pathlib.Path(temporary) / "compile.log"
            recorded = write_log(path, content)
            self.assertEqual(path.read_bytes(), content)
            self.assertEqual(recorded, hashlib.sha256(path.read_bytes()).hexdigest())

    def test_right_errors_from_wrong_path_reject(self):
        blocker = WINDOWS_BLOCKERS[0]
        stdout = b"".join(
            diagnostic(blocker, code, message, path="crates/other/src/lib.rs")
            for code, message in blocker.expected_errors
        ) + cargo_finished()
        accepted, _, _ = classify_windows_run(blocker, 101, stdout, b"")
        self.assertFalse(accepted)

    def test_right_errors_from_wrong_package_reject(self):
        blocker = WINDOWS_BLOCKERS[0]
        stdout = b"".join(
            diagnostic(blocker, code, message, package="other-crate")
            for code, message in blocker.expected_errors
        ) + cargo_finished()
        accepted, _, _ = classify_windows_run(blocker, 101, stdout, b"")
        self.assertFalse(accepted)


if __name__ == "__main__":
    unittest.main()
