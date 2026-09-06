#!/usr/bin/env python3
"""Actual truecolor JSON highlighting, scroll, horizontal pan, copy and cleanup."""
import base64
import pathlib
import re
import sys

from test_lvu_pty import PtyApp


def key_rgb(value):
    hashed = 0xCBF29CE484222325
    for byte in value.encode():
        hashed = ((hashed ^ byte) * 0x100000001B3) & ((1 << 64) - 1)
    hashed ^= hashed >> 33
    hashed = (hashed * 0xFF51AFD7ED558CCD) & ((1 << 64) - 1)
    hashed ^= hashed >> 33
    hashed = (hashed * 0xC4CEB9FE1A85EC53) & ((1 << 64) - 1)
    hashed ^= hashed >> 33
    hue = hashed / ((1 << 64) - 1) * 360.0
    saturation, lightness = 0.72, 0.68
    chroma = (1.0 - abs(2.0 * lightness - 1.0)) * saturation
    sector = hue / 60.0
    secondary = chroma * (1.0 - abs(sector % 2.0 - 1.0))
    values = [(chroma, secondary, 0), (secondary, chroma, 0), (0, chroma, secondary),
              (0, secondary, chroma), (secondary, 0, chroma), (chroma, 0, secondary)]
    red, green, blue = values[int(sector)]
    match_value = lightness - chroma / 2.0
    return tuple(int((channel + match_value) * 255.0 + 0.5) for channel in (red, green, blue))


def copy_text(app, text):
    app.wait_for(text)
    y, row = next((y, row) for y, row in enumerate(app.screen.display) if text in row)
    x = row.index(text)
    before = len(app.transcript)
    app.send(f"\x1b[<0;{x+1};{y+1}M".encode())
    app.send(f"\x1b[<32;{x+len(text)};{y+1}M".encode())
    app.send(f"\x1b[<0;{x+len(text)};{y+1}m".encode())
    app.send(b"\x03")
    app.wait_until(lambda _: b"\x1b]52;c;" in app.transcript[before:], "OSC52 copy")
    match = re.search(rb"\x1b\]52;c;([A-Za-z0-9+/=]*)(?:\x07|\x1b\\)", bytes(app.transcript[before:]))
    assert match
    assert base64.b64decode(match.group(1)).decode() == text


binary = pathlib.Path(sys.argv[1]).resolve()
app = PtyApp(binary, [], width=116, height=22, environment={
    "NO_COLOR": "", "COLORTERM": "truecolor", "LVU_NO_DELIGHT": "1",
})
try:
    app.wait_for("12:00:32")
    transcript = bytes(app.transcript)
    rgb = re.findall(rb"\x1b\[38;2;(\d+);(\d+);(\d+)(?:m|;)", transcript)
    assert len(set(rgb)) >= 6, "expected continuous key colors and semantic value contrast"

    app.send(b"g")
    app.wait_for("12:00:02")
    assert "12:00:32" not in app.text(), "vertical scroll did not move to early JSON"
    key_escape = f"\x1b[38;2;{';'.join(map(str, key_rgb('request_id')))}m".encode()
    key_prefix = key_escape[:-1]
    assert bytes(app.transcript).count(key_prefix) >= 2, "decoded equal keys did not reuse RGB identity"
    escaped = r'"request\u005fid"'
    escaped_y, escaped_row = next(
        (y, row) for y, row in enumerate(app.screen.display) if escaped in row
    )
    escaped_x = escaped_row.index(escaped)
    expected_hex = "".join(f"{channel:02x}" for channel in key_rgb("request_id"))
    assert app.screen.buffer[escaped_y][escaped_x].fg.lower() == expected_hex, (
        "escaped key did not render with decoded request_id identity color",
        app.screen.buffer[escaped_y][escaped_x].fg,
        expected_hex,
    )
    copy_text(app, escaped)

    app.send(b"\x1b[C" * 8)
    app.wait_for("COPY_JSON_MARKER")
    assert "x=64" in app.text(), "horizontal JSON pan did not advance"

    app.send(b"q")
    assert app.wait_exit(timeout=8) == 0
    app.assert_restored()
finally:
    if app.process.poll() is None:
        app.process.kill()
        app.process.wait()
    app.close()

print("JSON PTY passed: RGB roles, stable scroll/pan, decoded OSC52 copy, restoration")
