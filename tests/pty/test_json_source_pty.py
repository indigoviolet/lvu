#!/usr/bin/env python3
"""Check JSON colors and exact copy through actual file acquisition and rendering."""
import base64
import pathlib
import re
import sys
import tempfile

from test_lvu_pty import PtyApp


def locate(app, text):
    for y, row in enumerate(app.screen.display):
        if text in row:
            return row.index(text), y
    raise AssertionError(f"Missing {text!r}\n{app.text()}")


def foreground(app, text):
    x, y = locate(app, text)
    return app.screen.buffer[y][x].fg


def copy_exact(app, text):
    x, y = locate(app, text)
    before = len(app.transcript)
    app.send(f"\x1b[<0;{x+1};{y+1}M".encode())
    app.send(f"\x1b[<32;{x+len(text)};{y+1}M".encode())
    app.send(f"\x1b[<0;{x+len(text)};{y+1}m".encode())
    app.send(b"\x03")
    app.wait_until(lambda _: b"\x1b]52;c;" in app.transcript[before:], "OSC52 response")
    match = re.search(rb"\x1b\]52;c;([A-Za-z0-9+/=]*)(?:\x07|\x1b\\)",
                      bytes(app.transcript[before:]))
    assert match is not None
    assert base64.b64decode(match.group(1)).decode() == text
    assert app.process.poll() is None, "copy must not quit"


def run(binary):
    with tempfile.TemporaryDirectory(prefix="lvu-json-source-pty-") as directory:
        root = pathlib.Path(directory)
        config = root / "config/lvu"
        config.mkdir(parents=True)
        (config / "settings.toml").write_text(
            'schema_version = 1\n[appearance]\ntheme = "love-dark"\n'
            'delight_enabled = false\nreduced_motion = true\nascii = false\n'
            '[paseo]\nprovider = "codex/gpt-5.6-luna"\n'
            'mode = "full-access"\nthinking = "medium"\n'
            '[cache.memory]\nrows_mib = 4\nmembership_mib = 256\n'
            '[cache.disk]\ntotal_mib = 5120\nindex_per_source_mib = 256\n'
        )
        source = root / "events.jsonl"
        escaped = r'"request\u005fid"'
        records = []
        for index in range(32):
            key = escaped if index == 1 else '"request_id"'
            records.append(
                '{' + key + f':"EVENT_{index:02}","ok":true,"count":42,'
                '"message":"東京é ' + 'x' * 90 + ' COPY_JSON_SOURCE"}'
            )
        source.write_text("\n".join(records) + "\n")
        app = PtyApp(binary, [str(source), "--capture-dir", str(root / "capture")],
                     width=150, height=24, environment={
                         "XDG_CONFIG_HOME": str(root / "config"),
                         "XDG_DATA_HOME": str(root / "data"),
                         "XDG_CACHE_HOME": str(root / "cache"),
                         "NO_COLOR": "", "COLORTERM": "truecolor",
                     })
        try:
            app.wait_for("EVENT_31", timeout=10)
            app.send(b"g")
            app.wait_for("EVENT_00")
            app.wait_for("EVENT_02")
            # EVENT_00 is selected; compare two unselected rows instead.
            key_color = foreground(app, escaped)
            x, y = locate(app, '"EVENT_02"')
            row = app.screen.display[y]
            plain_x = row.index('"request_id"')
            assert key_color == app.screen.buffer[y][plain_x].fg, "decoded key identity changed"
            assert key_color != app.screen.buffer[y][x].fg, "key and string value lack distinct roles"
            assert re.fullmatch(r"[0-9a-fA-F]{6}", key_color), "expected truecolor key"
            copy_exact(app, escaped)
            app.send(b"\x1b[C" * 8)
            app.wait_for("COPY_JSON_SOURCE")
            assert "x=64" in app.text(), "horizontal pan did not advance"
            app.send(b"q")
            assert app.wait_exit(timeout=8) == 0
            app.assert_restored()
        finally:
            if app.process.poll() is None:
                app.process.kill()
                app.process.wait()
            app.close()
    print("JSON source PTY passed: real capture, decoded colors, lexical copy, pan, restoration")


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
