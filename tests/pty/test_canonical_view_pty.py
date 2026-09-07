#!/usr/bin/env python3
"""The permanent All events view: unfiltered, immutable, and the target of jumps.

Every source keeps one view whose definition cannot be edited. Filtering it
creates a new editable view instead, exactly one per editing burst, and only
when the query and the save both succeed. Bookmarks lead back to All events,
where the record is always present.
"""
import pathlib
import sqlite3
import sys
import tempfile
from test_lvu_pty import PtyApp
from test_enrichment_chain_pty import stop, paste

RECORDS = "".join(f"event {index:02d} {'alpha' if index % 2 else 'beta'}\n" for index in range(1, 13))


def launch(binary, source, root, environment, ready="event 12", width=120, height=26):
    app = PtyApp(
        binary,
        [str(source), "--capture-dir", str(root / "capture")],
        width=width,
        height=height,
        environment=environment,
    )
    app.wait_for(ready, timeout=15.0)
    return app


def view_lines(app):
    """View names as shown in the sources/views sidebar."""
    names = []
    for line in app.text().splitlines():
        if not line.startswith("│"):
            continue
        entry = line[1:21].replace("›", " ").strip()
        if not entry or entry.startswith(("●", "Running:")):
            continue
        if any(character in entry for character in "─│┌┐└┘▄▀"):
            continue
        names.append(entry)
    return names


def run(binary):
    with tempfile.TemporaryDirectory(prefix="lvu-canonical-pty-") as directory:
        root = pathlib.Path(directory)
        source = root / "events.log"
        source.write_text(RECORDS)
        environment = {
            "XDG_CONFIG_HOME": str(root / "config"),
            "XDG_DATA_HOME": str(root / "data"),
            "XDG_CACHE_HOME": str(root / "cache"),
        }

        app = launch(binary, source, root, environment)
        try:
            assert "All events" in app.text(), app.text()

            # Typing is only a draft on All events: the live search settles and
            # applies nothing, so no view appears mid-word.
            app.send(b"/")
            app.wait_for("Search")
            for byte in b"beta":
                app.send(bytes([byte]))
            app.assert_remains("No filter", "Applied", 1.0)
            assert "event 11 alpha" in app.text(), (
                "typing filtered All events in place",
                app.text(),
            )
            assert view_lines(app) == ["All events"], view_lines(app)

            # Applying it is what creates the view, once.
            app.send(b"\r")
            app.wait_until(
                lambda text: "event 11 alpha" not in text and "Applied   beta" in text,
                "the filter applied in the view it created",
                timeout=10,
            )
            app.wait_until(
                lambda text: view_lines(app).count("beta") == 1,
                "exactly one view was created",
                timeout=10,
            )
            app.send(b"\x1b")
            app.wait_until(lambda text: " Search " not in text, "search closed")
            names = view_lines(app)
            assert names.count("beta") == 1, ("one derived view per apply", names)
            assert "All events" in names, names

            # All events was not filtered in place: switching back shows
            # everything, and its own search is still empty.
            app.send(b"[")
            app.wait_until(
                lambda text: "event 11 alpha" in text and "event 12 beta" in text,
                "All events keeps every record",
                timeout=8,
            )
            app.send(b"/")
            app.wait_for("Search")
            app.wait_until(
                lambda text: "No filter" in text,
                "All events has no filter of its own",
            )
            app.send(b"\x1b")
            app.wait_until(lambda text: " Search " not in text, "search closed")

            # A rejected filter must leave no view behind. The advanced filter
            # is compiled, so a nonsense expression fails.
            before = view_lines(app)
            app.send(b"p")
            app.wait_for("Advanced")
            paste(app, "this is not a polars expression")
            app.send(b"\r")
            app.wait_until(
                lambda text: "Error" in text,
                "the rejection is reported",
                timeout=20,
            )
            app.send(b"\x1b")
            app.wait_until(lambda text: "Advanced" not in text, "advanced closed")
            assert view_lines(app) == before, (
                "a rejected filter created a view",
                before,
                view_lines(app),
            )

            # Presentation stays editable on All events.
            app.send(b"f")
            app.wait_until(lambda text: "FOLLOW" not in text, "follow toggles in place")
            assert view_lines(app) == before, ("presentation created a view", view_lines(app))

            # A bookmark taken in the filtered view leads back to All events,
            # even though its record is not in that view any more.
            app.send(b"]")
            app.wait_until(lambda text: "event 12 beta" in text and "event 11 alpha" not in text,
                           "back in the filtered view", timeout=8)
            app.send(b"g")
            app.send(b"b")
            app.wait_for("bookmarked")
            app.send(b"/")
            app.wait_for("Search")
            paste(app, "x")
            app.wait_until(
                lambda text: "event 02 beta" not in text,
                "the bookmarked record is filtered out of its own view",
                timeout=10,
            )
            app.send(b"\x1b")
            app.wait_until(lambda text: " Search " not in text, "search closed")
            app.send(b"B")
            app.wait_for("Bookmarks")
            # Explicit raw-context inspection is still reachable on its own
            # control, and still anchors on the bookmarked record.
            app.send(b"\t")
            app.wait_until(lambda text: "Raw context" in text, "raw context control")
            app.send(b"\r")
            app.wait_until(
                lambda text: "event 02 beta" in text and "event 01 alpha" in text,
                "raw context shows the record among its unfiltered neighbours",
                timeout=8,
            )
            app.send(b"\x1b")
            app.wait_until(lambda text: "Bookmarks" in text, "back in the bookmark list")
            # Activating the bookmark itself jumps instead of detouring.
            app.send(b"\x1b[Z")
            app.wait_until(lambda text: "Bookmarks" in text, "back on the bookmark list control")
            app.send(b"\r")
            app.wait_until(
                lambda text: "Bookmarks" not in text and "event 02 beta" in text,
                "the jump landed where the record still exists",
                timeout=8,
            )
            assert "event 11 alpha" in app.text(), (
                "the jump landed in the unfiltered view",
                app.text(),
            )
            restored_names = view_lines(app)
            stop(app)
        finally:
            if app.process.poll() is None:
                app.process.kill()
                app.process.wait()
                app.close()

        # Everything survives a restart: the canonical view, the views its
        # edits created, and their definitions.
        app = launch(binary, source, root, environment)
        try:
            app.wait_until(
                lambda text: "All events" in text and "beta" in text,
                "both views restored",
                timeout=15,
            )
            names = view_lines(app)
            # The rejected advanced filter and every superseded candidate left
            # nothing behind, in the workspace as well as on screen.
            assert sorted(names) == ["All events", "beta"], (names, restored_names)
            # A restart reopens whichever view was last in use. The last thing
            # this session did was jump to All events, so that is where it
            # reopens: the rule follows the user, not the newest view.
            assert "› All events" in app.text(), app.text()
            with sqlite3.connect(root / "capture" / "workspace" / "workspace.sqlite3") as connection:
                persisted = connection.execute(
                    "SELECT name,role FROM working_views ORDER BY name"
                ).fetchall()
            assert persisted == [("All events", "canonical"), ("beta", "derived")], persisted
            assert "event 11 alpha" in app.text(), "restart opens on the unfiltered view"
            app.send(b"]")
            # The derived view kept the exact definition it ended the session
            # with, which matched nothing.
            app.wait_until(
                lambda text: "No matches" in text,
                "the derived view kept its filter across restart",
                timeout=10,
            )
            # And unlike All events it is edited in place: narrowing it back
            # changes this view rather than creating another one.
            app.send(b"/")
            app.wait_for("Search")
            app.wait_until(lambda text: "Applied   betax" in text, "the derived view owns its filter")
            app.send(b"\x7f")
            app.wait_until(
                lambda text: "Applied   beta" in text and "Applied   betax" not in text
                and "No matches" not in text,
                "editing a derived view edits it in place",
                timeout=10,
            )
            app.send(b"\x1b")
            app.wait_until(lambda text: " Search " not in text, "search closed")
            names = view_lines(app)
            assert names.count("beta") == 1 and len(names) == 2, names
            stop(app)
        finally:
            if app.process.poll() is None:
                app.process.kill()
                app.process.wait()
                app.close()

        # Migration: a workspace that predates roles has no canonical view, and
        # its views must survive exactly as the user left them.
        database = root / "capture" / "workspace" / "workspace.sqlite3"
        assert database.is_file(), database
        with sqlite3.connect(database) as connection:
            connection.execute(
                "UPDATE working_views SET role='derived', name='My working view',"
                " applied_search='alpha' WHERE role='canonical'"
            )
            connection.commit()
            rows = connection.execute(
                "SELECT name,role,applied_search FROM working_views ORDER BY name"
            ).fetchall()
        assert all(role == "derived" for _, role, _ in rows), rows

        app = launch(binary, source, root, environment, ready="My working view")
        try:
            app.wait_until(
                lambda text: "My working view" in text and "All events" in text,
                "a canonical view is created beside the views that already existed",
                timeout=15,
            )
            names = view_lines(app)
            assert sorted(names) == ["All events", "My working view", "beta"], names
            # The pre-existing view keeps the definition it was migrated with,
            # and is still editable.
            # The restart reopened the view last in use, so step back to the
            # migrated one.
            app.send(b"[")
            app.wait_until(lambda text: "› All events" in text,
                           "back to the unfiltered view", timeout=8)
            app.send(b"[")
            app.wait_until(
                lambda text: "› My working view" in text and "event 11 alpha" in text
                and "event 12 beta" not in text,
                "the migrated view kept its own filter",
                timeout=10,
            )
            stop(app)
        finally:
            if app.process.poll() is None:
                app.process.kill()
                app.process.wait()
                app.close()

    print(
        "Canonical view PTY passed: All events stays unfiltered, drafts apply nowhere until submitted, "
        "one view per apply, rejected filters leave nothing, bookmarks jump home, migration preserves views, "
        "and all of it survives restart"
    )


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
