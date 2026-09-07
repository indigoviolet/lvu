#!/usr/bin/env python3
"""Identity colors on a terminal that never claimed truecolor.

`lvu --demo json` colors each JSON key by a hash of the key, so the same key is
always the same color. Those colors used to be emitted as 24-bit `38;2;R;G;B`
whatever the terminal was; a 256-color emulator then approximated them itself,
which is how two keys can end up looking identical and how a color lvu measured
as readable can land somewhere it never checked. With `COLORTERM` unset lvu must
choose from the palette the terminal actually has and say so in the sequence it
writes.

Nothing here re-implements lvu's color maths: the truecolor run supplies the
expectation that the 256-color run must *not* contain, and the cube arithmetic
used to read an index back is the fixed xterm palette, not lvu's choice within
it.
"""
import pathlib
import re
import sys

from test_lvu_pty import PtyApp

CUBE_LEVELS = (0, 95, 135, 175, 215, 255)
KEY = "request_id"
# The same key spelled with an escape; it decodes to KEY and must share its color.
ESCAPED_KEY = r'"request\u005fid"'


def cube_hex(index: int) -> str:
    """The color xterm displays for a 6x6x6 cube index, as pyte spells it."""
    offset = index - 16
    channels = (CUBE_LEVELS[offset // 36], CUBE_LEVELS[(offset // 6) % 6], CUBE_LEVELS[offset % 6])
    return "".join(f"{channel:02x}" for channel in channels)


def start(binary: pathlib.Path, colorterm: str | None) -> PtyApp:
    environment = {"NO_COLOR": "", "LVU_NO_DELIGHT": "1"}
    # Unset rather than empty: an absent COLORTERM is what a plain
    # `xterm-256color` session actually looks like.
    environment["COLORTERM"] = colorterm if colorterm is not None else ""
    app = PtyApp(binary, ["--demo", "json"], width=116, height=22, environment=environment)
    app.wait_for("12:00:32", timeout=15.0)
    app.send(b"g")
    app.wait_for("12:00:02", timeout=15.0)
    app.wait_for(ESCAPED_KEY, timeout=15.0)
    return app


def key_color(app: PtyApp) -> str:
    """The identity color shared by every rendering of the key.

    The selected row draws its whole line in the selection role, so it is not an
    identity color and is excluded: a color that only one row uses cannot be the
    hashed one. Every remaining row carrying the key -- including the escaped
    spelling, which decodes to the same key -- must agree, and that agreement is
    the identity property the hash exists to provide.
    """
    seen = {}
    for y, row in enumerate(app.screen.display):
        for needle in (f'"{KEY}"', ESCAPED_KEY):
            at = row.find(needle)
            if at >= 0:
                seen.setdefault(app.screen.buffer[y][at + 1].fg.lower(), []).append((y, needle))
    shared = {color: rows for color, rows in seen.items() if len(rows) > 1}
    assert len(shared) == 1, (
        f"expected one shared key identity color, saw {seen}\n{app.text()}"
    )
    color, rows = next(iter(shared.items()))
    assert any(needle == ESCAPED_KEY for _, needle in rows), (
        f"the escaped spelling did not share the decoded key's color: {seen}"
    )
    return color


def finish(app: PtyApp) -> None:
    app.send(b"q")
    assert app.wait_exit(timeout=8) == 0, "the fixture did not exit cleanly"
    app.assert_restored()


def main() -> None:
    binary = pathlib.Path(sys.argv[1]).resolve()

    truecolor = start(binary, "truecolor")
    try:
        plain = key_color(truecolor)
        wide_transcript = bytes(truecolor.transcript)
        red, green, blue = (int(plain[at:at + 2], 16) for at in (0, 2, 4))
        wide_sequence = f"38;2;{red};{green};{blue}".encode()
        assert wide_sequence in wide_transcript, \
            f"the truecolor run did not emit {wide_sequence!r} for the key color"
        assert not re.search(rb"\x1b\[[0-9;]*38;5;", wide_transcript), \
            "a truecolor terminal must not be sent 256-color sequences"
        finish(truecolor)
    finally:
        if truecolor.process.poll() is None:
            truecolor.process.kill()
            truecolor.process.wait()
        truecolor.close()

    indexed = start(binary, None)
    try:
        narrow_transcript = bytes(indexed.transcript)
        emitted = {int(index) for index in re.findall(rb"38;5;(\d+)", narrow_transcript)}
        cube = {index for index in emitted if 16 <= index < 232}
        assert len(cube) >= 2, \
            f"expected several color-cube identity colors, saw {sorted(emitted)}"

        plain = key_color(indexed)
        # The rendered color must be one the terminal really has, and lvu must
        # have named it by index rather than describing it in 24 bits.
        matching = [index for index in cube if cube_hex(index) == plain]
        assert matching, \
            f"the key rendered {plain}, which is not any emitted cube index {sorted(cube)}"
        assert wide_sequence not in narrow_transcript, \
            f"the 256-color run still emitted the truecolor key color {wide_sequence!r}"
        assert plain != f"{red:02x}{green:02x}{blue:02x}", \
            "the downgrade left the key color unchanged, so nothing was quantised"
        finish(indexed)
    finally:
        if indexed.process.poll() is None:
            indexed.process.kill()
            indexed.process.wait()
        indexed.close()

    print("Color depth PTY passed: identity colors are 24-bit with COLORTERM=truecolor "
          f"and color-cube indexes without it (key {KEY!r}: {wide_sequence.decode()} "
          f"becomes 38;5;{matching[0]})")


if __name__ == "__main__":
    main()
