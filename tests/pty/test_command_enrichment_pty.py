#!/usr/bin/env python3
"""Actual-app command enrichment: reviewed delivery, persistence, and rollback."""

from __future__ import annotations

import json
import os
import pathlib
import sqlite3
import sys
import tempfile
import time

from test_lvu_pty import PtyApp


CTRL_S = b"\x13"
CTRL_R = b"\x12"
ALT_N = b"\x1bn"
ALT_C = b"\x1bc"

NATIVE = r'/"request":"(?P<native_request>r-\d+)"/'

HELPER = r'''#!/usr/bin/env python3
import json
import pathlib
import sys

marker = pathlib.Path(sys.argv[1])
mode = sys.argv[2]
batch = None
events = []
for line in sys.stdin:
    item = json.loads(line)
    if item["type"] == "batch_begin":
        batch = item
        events = []
        with marker.open("a", encoding="utf-8") as out:
            out.write(json.dumps({"kind": "batch", "count": item["event_count"]}) + "\n")
            out.flush()
    elif item["type"] == "event":
        events.append(item)
        with marker.open("a", encoding="utf-8") as out:
            out.write(json.dumps({
                "kind": "event",
                "id": item["event_id"],
                "native_request": item["fields"].get("native_request"),
            }, sort_keys=True) + "\n")
            out.flush()
    elif item["type"] == "batch_end":
        if mode == "malformed":
            print("{malformed", flush=True)
            continue
        for event in reversed(events):
            result = {
                "type": "event",
                "session": batch["session"],
                "revision": batch["revision"],
                "event_id": event["event_id"],
                "fields": {
                    "seen_sequence": event["event_id"]["sequence"],
                    "native_copy": event["fields"].get("native_request"),
                    "typed_ok": True,
                },
            }
            print(json.dumps(result), flush=True)
        print(json.dumps({
            "type": "batch_complete",
            "session": batch["session"],
            "revision": batch["revision"],
        }), flush=True)
'''


def paste(app: PtyApp, text: str) -> None:
    app.send(b"\x1b[200~" + text.encode() + b"\x1b[201~")


def marker_rows(path: pathlib.Path) -> list[dict]:
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text().splitlines() if line]


def wait_marker(app: PtyApp, path: pathlib.Path, event_count: int, label: str) -> list[dict]:
    rows: list[dict] = []

    def ready(_: str) -> bool:
        nonlocal rows
        rows = marker_rows(path)
        return sum(row["kind"] == "event" for row in rows) >= event_count

    app.wait_until(ready, label, timeout=15)
    return rows


def open_command(app: PtyApp) -> None:
    app.send(b"e")
    app.wait_for("┌ Enrichment ")
    app.send(ALT_C)
    app.wait_for("External command ")


def close_command(app: PtyApp) -> None:
    app.send(b"\x1b")
    app.wait_until(
        lambda text: "External command " not in text and "? help" in text,
        "close command dialog and restore workspace footer",
    )


def inspect_command_details(app: PtyApp, labels: tuple[str, ...] = (
    "command.status", "command.native_copy", "command.seen_sequence", "command.typed_ok",
)) -> str:
    # Scroll the actual pane, retaining evidence from each visible viewport.
    # Reopen Details to establish its explicit pane focus, then use arrows.
    app.send(b"dd" + b"\x1b[B" * 16)
    seen = app.wait_for("Selected event details", timeout=10)
    for _ in range(32):
        if all(label in seen for label in labels):
            app.send(b"\t\t")  # Details -> Selector -> Logs for the next workspace action.
            return seen
        app.send(b"\x1b[B")  # Down in the focused Details pane.
        time.sleep(0.1)
        app.drain()
        seen += "\n" + app.text()
    raise AssertionError("Details scrolling did not expose all command fields:\n" + seen)


def save_command(app: PtyApp) -> None:
    app.send(CTRL_S)
    app.wait_until(
        lambda text: "Saved" in text and "Unrun" in text,
        "durably saved command definition without execution",
        timeout=12,
    )


def review_run(app: PtyApp) -> None:
    app.send(CTRL_R)
    review = app.wait_for("Run review", timeout=12)
    assert "1,024 records / 4 MiB input; no sampling" in review


def stop(app: PtyApp) -> None:
    if app.process.poll() is None:
        app.send(b"\x03")
        assert app.wait_exit(timeout=10) == 0
    app.assert_restored()
    app.close()


def configure_command(app: PtyApp, helper: pathlib.Path, marker: pathlib.Path, mode: str) -> None:
    open_command(app)
    paste(app, sys.executable)
    app.send(b"\t")
    paste(app, str(helper))
    app.send(ALT_N)
    paste(app, str(marker))
    app.send(ALT_N)
    paste(app, mode)
    save_command(app)


def run(binary: pathlib.Path) -> None:
    with tempfile.TemporaryDirectory(prefix="lvu-command-enrichment-pty-") as temporary:
        root = pathlib.Path(temporary)
        source = root / "events.jsonl"
        capture = root / "capture"
        marker = root / "delivered.jsonl"
        helper = root / "command_helper.py"
        helper.write_text(HELPER)
        helper.chmod(0o700)
        original = [
            {"request": "r-1", "status": 503, "message": "alpha"},
            {"request": "r-2", "status": 200, "message": "beta"},
        ]
        source.write_text("".join(json.dumps(row, separators=(",", ":")) + "\n" for row in original))
        environment = {
            "MISE_DATA_DIR": os.environ.get("MISE_DATA_DIR", str(pathlib.Path.home() / ".local/share/mise")),
            "UV_CACHE_DIR": os.environ.get("UV_CACHE_DIR", str(pathlib.Path.home() / ".cache/uv")),
            "XDG_CONFIG_HOME": str(root / "config"),
            "XDG_DATA_HOME": str(root / "data"),
            "XDG_CACHE_HOME": str(root / "cache"),
            "NO_COLOR": "",
            "COLORTERM": "truecolor",
            "LVU_NO_DELIGHT": "1",
        }
        settings = root / "config" / "lvu" / "settings.toml"
        settings.parent.mkdir(parents=True)
        settings.write_text('''schema_version = 1
[paseo]
provider = "codex/gpt-5.6-luna"
mode = "full-access"
thinking = "medium"
[appearance]
theme = "love-dark"
delight_enabled = false
reduced_motion = true
ascii = false
[cache.memory]
rows_mib = 4
membership_mib = 256
[cache.disk]
total_mib = 5120
index_per_source_mib = 256
''')
        arguments = [str(source), "--capture-dir", str(capture)]

        app = PtyApp(binary, arguments, width=150, height=38, environment=environment)
        try:
            app.wait_for('"request":"r-2"', timeout=8)
            app.send(b"e")
            app.wait_for("┌ Enrichment ")
            # The step draft moved into the nested step editor with the
            # two-layer rework; the list layer has no editable field.
            app.send(b"\x1ba")
            app.wait_for("Enrichment › New step")
            paste(app, NATIVE)
            app.send(b"\r")
            # The saved step returns to the list layer, which is where the
            # "External command" pane heading is.
            app.wait_until(lambda text: '1  /"request"' in text
                           and "1 steps active" in text,
                           "saved native step", timeout=15)
            app.send(b"\x1b")
            # ESC immediately followed by a printable byte parses as Alt-<key>,
            # so wait for the list to close before the next shortcut.
            app.wait_until(lambda text: "┌ Enrichment" not in text, "list closed")

            configure_command(app, helper, marker, "valid")
            assert marker_rows(marker) == [], "saving a definition delivered records"

            review_run(app)
            app.send(b"\x1b")
            app.wait_until(lambda text: "External command " not in text, "cancelled run review")
            time.sleep(0.2)
            app.drain()
            assert marker_rows(marker) == [], "Escape from review delivered records"

            open_command(app)
            review_run(app)
            app.send(b"\r")
            app.wait_for("Published", timeout=15)
            first_delivery = wait_marker(app, marker, 2, "first reviewed command delivery")
            delivered = [row for row in first_delivery if row["kind"] == "event"]
            assert len({(row["id"]["source_id"], row["id"]["sequence"]) for row in delivered}) == 2
            assert {row["native_request"] for row in delivered} == {"r-1", "r-2"}
            close_command(app)
            app.send(b"d")
            details = inspect_command_details(app)
            assert '"request":"r-2"' in details and "native_request" in details

            with source.open("a") as out:
                out.write(json.dumps({"request": "r-3", "status": 418, "message": "new " + "界e\u0301" * 160 + " raw-tail"}, separators=(",", ":"), ensure_ascii=False) + "\n")
                out.flush()
                os.fsync(out.fileno())
            app.wait_for('"request":"r-3"', timeout=12)
            pending = inspect_command_details(app, ("command.status", "Pending"))
            assert "native_request" in pending and "raw-tail" in pending
            before_second_run = marker_rows(marker)
            time.sleep(0.2)
            app.drain()
            assert marker_rows(marker) == before_second_run, "new arrival ran automatically"

            open_command(app)
            review_run(app)
            app.send(b"\r")
            app.wait_for("Published", timeout=15)
            second_delivery = wait_marker(app, marker, 3, "explicit delivery of pending record")
            new_events = [row for row in second_delivery[len(before_second_run):] if row["kind"] == "event"]
            assert len(new_events) == 1 and new_events[0]["native_request"] == "r-3"
            close_command(app)

            # Hold only the fixture database writer while a fully reused run
            # reaches its save boundary. Close must not cancel an accepted save.
            open_command(app)
            review_run(app)
            before_reused_run = marker_rows(marker)
            with sqlite3.connect(capture / "workspace" / "workspace.sqlite3", timeout=5) as blocker:
                blocker.execute("BEGIN IMMEDIATE")
                try:
                    app.send(b"\r")
                    saving = app.wait_for("Saving results", timeout=8)
                    # §7.4 replaced `Status: <word>` with the shared message row.
                    assert "Saving results" in saving and "Esc cancel" not in saving
                    close_command(app)
                finally:
                    blocker.rollback()
            app.wait_for("Published 3 durable command results", timeout=8)
            assert marker_rows(marker) == before_reused_run, "reused run redelivered records"

            # Narrow rendering keeps the focused field and action discovery colored.
            open_command(app)
            app.resize(54, 18)
            narrow = app.wait_until(
                lambda text: "Program" in text and all(label in text for label in ("Save", "Review", "Remove")),
                "narrow command form with actions",
            )
            assert "External command " in narrow
            app.drain()
            transcript = bytes(app.transcript)
            assert b"\x1b[38;2;" in transcript or b"\x1b[48;2;" in transcript, (
                "command dialog did not emit an RGB semantic style"
            )
            cursor = app.screen.cursor
            assert app.screen.buffer[cursor.y][cursor.x].bg != "default"
            # §4.2 puts the label and its field on one row, in two columns.
            program_row = next(y for y, line in enumerate(narrow.splitlines()) if "Program" in line)
            program_column = narrow.splitlines()[program_row].index("Program")
            label_cell = app.screen.buffer[program_row][program_column]
            input_cell = app.screen.buffer[program_row][program_column + 14]
            assert label_cell.fg != "default", "Program label did not use the accent role"
            assert input_cell.bg != "default" and input_cell.bg != label_cell.bg, (
                "editable surface did not use its distinct input background"
            )
            app.resize(150, 38)
            close_command(app)
            stop(app)
        finally:
            if app.process.poll() is None:
                app.process.kill()
                app.process.wait(timeout=5)
                app.close()

        delivered_before_reopen = marker_rows(marker)
        reopened = PtyApp(binary, arguments, width=150, height=38, environment=environment)
        try:
            reopened.wait_for('"request":"r-3"', timeout=15)
            reopened.send(b"d")
            restored = inspect_command_details(reopened)
            assert "Ready" in restored and "r-3" in restored
            time.sleep(0.2)
            reopened.drain()
            assert marker_rows(marker) == delivered_before_reopen, "restore launched saved command"

            # Save a new definition whose protocol fails; old published fields remain.
            open_command(reopened)
            reopened.send(b"\t")
            reopened.send(b"\x7f" * len(f"{helper}\n{marker}\nvalid"))
            # §12.6 paints the field's lines; an emptied field shows its
            # placeholder, so the cleared content going away is the marker.
            reopened.wait_until(lambda text: "valid" not in text, "arguments cleared")
            paste(reopened, str(helper))
            reopened.send(ALT_N)
            paste(reopened, str(marker))
            reopened.send(ALT_N)
            paste(reopened, "malformed")
            reopened.wait_for("malformed")
            save_command(reopened)
            review_run(reopened)
            reopened.send(b"\r")
            failed = reopened.wait_until(
                lambda text: "Error" in text and ("malformed" in text or "protocol" in text),
                "protocol failure diagnostic",
                timeout=15,
            )
            assert "Previous published results retained" in failed
            reopened.send(b"\x1b")
            reopened.wait_until(lambda text: "External command " not in text, "failed command dialog closed")
            if "Selected event details" not in reopened.text():
                reopened.send(b"d")
            retained = inspect_command_details(reopened)
            assert "command.status" in retained and "Ready" in retained
            # Save then immediately quit must settle metadata without another run.
            deliveries_before_quit = marker_rows(marker)
            open_command(reopened)
            reopened.send(b"\t\t\t")
            paste(reopened, "FINAL_SAVE=yes")
            reopened.send(b"\x13\x03")
            assert reopened.wait_exit(timeout=10) == 0
            reopened.assert_restored()
            reopened.close()
            assert marker_rows(marker) == deliveries_before_quit
            database = capture / "workspace" / "workspace.sqlite3"
            with sqlite3.connect(database) as connection:
                states = [json.loads(row[0]) for row in connection.execute(
                    "SELECT presentation_json FROM working_views"
                )]
            assert any((state.get("command_enrichment") or {}).get("definition", {}).get(
                "environment", {}
            ).get("FINAL_SAVE") == "yes" for state in states), "quit lost the explicit save"
            assert any(state.get("command_publication") for state in states), (
                "definition edits erased the last durable command publication"
            )
        finally:
            if reopened.process.poll() is None:
                reopened.process.kill()
                reopened.process.wait(timeout=5)
                reopened.close()


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
    print("Command enrichment PTY passed: explicit review/delivery, native input, pending arrivals, persistence, rollback, narrow color and restoration")
