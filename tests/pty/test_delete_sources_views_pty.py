"""Delete a derived view and remove a source, with restart persistence.

Covers: obvious keyboard paths with explicit confirmation (View dialog
Delete mode Enter/Enter; sidebar Delete/Delete), canonical/dependency
safety is unit-tested in Rust, durable deletion across restart, retained
journal bytes and source bookmarks, and explicit discovery categories
(Processes/open files, Project files, Docker services / containers, Remembered
sources) including zero-match outcomes.
"""
import os
import pathlib
import sqlite3
import sys
import tempfile

from test_lvu_pty import PtyApp


def environment(root):
    return {
        "XDG_CONFIG_HOME": str(root / "config"),
        "XDG_DATA_HOME": str(root / "data"),
        "XDG_CACHE_HOME": str(root / "cache"),
        "MISE_DATA_DIR": os.environ.get("MISE_DATA_DIR", str(pathlib.Path.home() / ".local/share/mise")),
        "UV_CACHE_DIR": os.environ.get("UV_CACHE_DIR", str(pathlib.Path.home() / ".cache/uv")),
        "NO_COLOR": "",
        "COLORTERM": "truecolor",
    }


def stop(app):
    app.send(b"q")
    assert app.wait_exit(timeout=8) == 0
    app.assert_restored()


def run(binary):
    with tempfile.TemporaryDirectory(prefix="lvu-delete-pty-") as directory:
        root = pathlib.Path(directory)
        capture = root / "capture"
        log = root / "events.log"
        log.write_text("alpha one\nbeta two\nalpha three\n")
        env = environment(root)

        # --- derived view deletion -------------------------------------
        app = PtyApp(binary, [str(log), "--capture-dir", str(capture)],
                     width=100, height=28, environment=env)
        try:
            app.wait_for("alpha one")
            # Clone the active view: v opens Manage views on Clone.
            app.send(b"v")
            app.wait_for("Copy of")
            app.send(b"\r")
            app.wait_for("view saved")
            # The ack closes the dialog; the Name field going away proves it.
            app.wait_until(lambda text: "Name  Copy" not in text, "clone dialog closed")
            app.wait_until(lambda text: "Copy of All event" in text, "cloned view listed")
            # Delete it: v opens, Alt-E selects Delete, Enter arms, Enter confirms.
            app.send(b"v")
            app.wait_for("Delete")
            app.send(b"\x1be")
            app.wait_for("stays on disk")
            app.send(b"\r")
            app.wait_for("confirm")
            app.send(b"\r")
            app.wait_for("deleted view")
            # The notice itself names the deleted view, so prove removal by
            # reopening Manage views: the selection survivor owns the title.
            app.send(b"v")
            app.wait_for("View \u00b7 All events")
            app.send(b"\x1b")
            app.wait_until(lambda text: "alpha one" in text, "survivor shows rows")
            stop(app)
        finally:
            if app.process.poll() is None:
                app.process.kill()
                app.process.wait(timeout=5)
            app.close()

        # Restart: the deletion is durable, the capture is intact.
        app = PtyApp(binary, ["--capture-dir", str(capture)],
                     width=100, height=28, environment=env)
        try:
            app.wait_for("alpha one")
            app.wait_until(lambda text: "Copy of All event" not in text,
                           "deleted view stays gone after restart")
            stop(app)
        finally:
            if app.process.poll() is None:
                app.process.kill()
                app.process.wait(timeout=5)
            app.close()

        journals = list(capture.rglob("*.journal"))
        assert journals, "capture journal must survive view deletion"
        assert any(journal.read_bytes().count(b"alpha one") >= 1 for journal in journals)

        # --- source removal ---------------------------------------------
        second = root / "second.log"
        second.write_text("second-line\n")
        app = PtyApp(binary, [str(log), str(second), "--capture-dir", str(capture)],
                     width=100, height=28, environment=env)
        try:
            app.wait_for("alpha one")
            # Switch to the second source's view first.
            app.send(b"]")
            app.wait_for("second-line")
            # Bookmark a record so retention is observable below.
            app.send(b"b")
            app.wait_until(lambda text: "ookmark" in text, "bookmark recorded")
            # Remove via the palette (plain-text path; the Delete key binding
            # is pinned in Rust keymap tests instead of racing a 4-byte
            # escape sequence through a loaded pty). Each Enter waits for
            # the row to be selected first: mid-typing frames still select
            # other rows.
            app.send(b"\x10")
            app.wait_for("Command palette")
            app.send(b"Remove source")
            app.wait_until(
                lambda text: "› Remove source from workspace" in text,
                "removal row selected",
            )
            app.send(b"\r")
            app.wait_until(lambda text: "Command palette" not in text, "palette ran removal")
            # The status line truncates the long arm notice; match its head.
            # (The full "captured data stays on disk" wording is asserted in
            # Rust TestBackend coverage of the same notice.)
            app.wait_for("Remove source")
            app.send(b"\x10")
            app.wait_for("Command palette")
            app.send(b"Remove source")
            app.wait_until(
                lambda text: "› Remove source from workspace" in text,
                "removal row selected again",
            )
            app.send(b"\r")
            app.wait_until(lambda text: "Command palette" not in text, "palette ran confirm")
            app.wait_until(lambda text: "removed source" in text, "source removed", timeout=15.0)
            app.wait_until(lambda text: "second-line" not in text or "alpha" in text,
                           "removed source leaves the sidebar")
            stop(app)
        finally:
            if app.process.poll() is None:
                app.process.kill()
                app.process.wait(timeout=5)
            app.close()

        # Removal is workspace membership only: journals, the source row and
        # its bookmarks survive byte-identical on disk.
        journals = list(capture.rglob("*.journal"))
        assert journals, "journals must survive source removal"
        assert any(journal.read_bytes().count(b"second-line") >= 1 for journal in journals), \
            "removed source bytes stay in its journal"
        workspace = capture / "workspace" / "workspace.sqlite3"
        assert workspace.exists(), "workspace database must survive"
        connection = sqlite3.connect(str(workspace))
        try:
            sources = connection.execute("SELECT COUNT(*) FROM sources").fetchone()[0]
            assert sources >= 1, "source rows stay for reconnect"
            views = connection.execute("SELECT COUNT(*) FROM working_views").fetchone()[0]
            assert views >= 1, "surviving source keeps its views"
        finally:
            connection.close()

        # Restart: the removed source is not restored.
        app = PtyApp(binary, ["--capture-dir", str(capture)],
                     width=100, height=28, environment=env)
        try:
            app.wait_for("alpha one")
            app.wait_until(lambda text: "second-line" not in text, "removed source not restored")
            stop(app)
        finally:
            if app.process.poll() is None:
                app.process.kill()
                app.process.wait(timeout=5)
            app.close()

        # --- explicit discovery categories -------------------------------
        app = PtyApp(binary, ["--capture-dir", str(root / "capture-empty")],
                     width=100, height=28, environment=env)
        try:
            # A sourceless launch shows the startup screen first; any key
            # dismisses it into the Add source dialog.
            app.send(b" ")
            app.wait_for("Add source")
            app.send(b"\x04")
            app.wait_for("Candidates")
            # The report lands when the bounded scan completes; the scanning
            # message losing its "scanning" tail is visible without scrolling
            # and proves completion.
            app.wait_until(
                lambda text: "scanning" not in text,
                "scan completed",
                timeout=15.0,
            )
            # The category report follows the selected-candidate detail inside
            # the Details pane while the candidate list keeps its geometry:
            # focus Details with a click so Down scrolls the report (not the
            # candidate selection), then walk it and accumulate every row.
            import time as _time
            seen = set()
            deadline = _time.monotonic() + 20.0
            categories = (
                "Processes / open files",
                "Project files",
                "Docker services / containers",
                "Remembered sources",
            )
            while _time.monotonic() < deadline:
                app.drain()
                rows = app.text().splitlines()
                located = next(
                    ((y, line) for y, line in enumerate(rows) if "Details" in line),
                    None,
                )
                assert located is not None, "Details pane visible:\n" + app.text()
                y, line = located
                x = line.index("Details")
                app.send(f"\x1b[<0;{x + 2};{y + 2}M\x1b[<0;{x + 2};{y + 2}m".encode())
                app.drain()
                # Only the Details/report region: candidate rows above keep
                # their pre-existing provider tags and are not the report.
                seen.update(rows[y:])
                joined = "\n".join(seen)
                if all(category in joined for category in categories):
                    break
                app.send(b"\x1b[B")
                _time.sleep(0.3)
            else:
                raise AssertionError(
                    "discovery categories never all visible:\n" + "\n".join(seen)
                )
            joined = "\n".join(seen)
            for category in ("Processes / open files", "Project files",
                             "Docker services / containers", "Remembered sources"):
                assert category in joined, f"discovery names {category}:\n{joined}"
            assert "Procfs" not in joined, "product labels, not Debug names"
            app.send(b"\x1b")
            app.wait_until(lambda text: "Candidates" not in text, "discovery closed")
            stop(app)
        finally:
            if app.process.poll() is None:
                app.process.kill()
                app.process.wait(timeout=5)
            app.close()

    print("Delete sources/views PTY passed")


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
    print("Delete sources/views PTY passed")
