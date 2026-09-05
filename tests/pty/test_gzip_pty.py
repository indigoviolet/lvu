#!/usr/bin/env python3
"""Open gzip files through the real CLI, then reopen without duplicate capture."""

from __future__ import annotations

import gzip
import pathlib
import shutil
import sys
import tempfile

from test_lvu_pty import PtyApp


def run_case(binary: pathlib.Path, name: str, encoded: bytes) -> None:
    with tempfile.TemporaryDirectory(prefix="lvu-gzip-pty-") as temporary:
        root = pathlib.Path(temporary)
        source = root / name
        source.write_bytes(encoded)
        capture = root / "capture"
        arguments = [str(source), "--capture-dir", str(capture)]
        previous_journals = None
        for _ in range(2):
            app = PtyApp(binary, arguments, width=130, height=26, cwd=root, environment={
                "XDG_CONFIG_HOME": str(root / "config"),
                "XDG_CACHE_HOME": str(root / "cache"),
                "XDG_DATA_HOME": str(root / "data"),
            })
            try:
                app.wait_for("first gzip event", timeout=10)
                app.wait_for("last gzip event", timeout=10)
                app.send(b"q")
                assert app.wait_exit(timeout=8) == 0
                app.assert_restored()
            except BaseException:
                evidence = pathlib.Path(tempfile.mkdtemp(prefix="lvu-gzip-failure-"))
                shutil.copytree(root, evidence, dirs_exist_ok=True)
                (evidence / "screen.txt").write_text(app.text())
                print(f"gzip failure evidence: {evidence}", flush=True)
                raise
            finally:
                if app.process.poll() is None:
                    app.process.kill()
                    app.process.wait(timeout=5)
                app.close()
            journals = {str(path.relative_to(capture)): path.read_bytes()
                        for path in capture.rglob("*.journal")}
            assert journals, "no durable journal was created"
            captured = b"".join(journals.values())
            assert b"first gzip event\xff" in captured
            assert b"last gzip event" in captured
            assert source.read_bytes() == encoded, "opening changed the original file"
            if previous_journals is not None:
                assert journals == previous_journals, "reopening duplicated captured records"
            previous_journals = journals


def main() -> None:
    binary = pathlib.Path(sys.argv[1]).resolve()
    decoded = b"first gzip event\xff\r\nlast gzip event\n"
    run_case(binary, "service.log.gz", gzip.compress(decoded, mtime=0))
    run_case(binary, "archive without extension", gzip.compress(decoded, mtime=0))
    run_case(binary, "plain.log.gz", decoded)
    run_case(binary, "members.log.gz", gzip.compress(b"first gzip event\xff\r\n", mtime=0)
             + gzip.compress(b"last gzip event\n", mtime=0))
    print("Gzip PTY passed: magic detection, plain .gz, concatenated members, exact bytes, unchanged archives, duplicate-free reopen, terminal restoration")


if __name__ == "__main__":
    main()
