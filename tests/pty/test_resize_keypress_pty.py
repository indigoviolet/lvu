#!/usr/bin/env python3
"""A resize followed straight away by a keypress must not wedge the app.

The reported failure is a live process that stops responding and leaves the
alternate screen on, so the user's shell needs `reset`. Its cause is on the
input path, not in any dialog: crossterm reads the terminal by looping `read(2)`
until its parser produces an event and stops only on `WouldBlock`, which a
blocking descriptor never reports. A read that delivers just the start of an
escape sequence therefore sends it back into `read(2)`, where it sleeps with the
poll timeout unexpired, the SIGWINCH pipe undrained, no frame emitted and the
terminal never restored.

Terminals split escape sequences across writes: a multiplexer, an ssh link or a
loaded machine can hand over `ESC [` and the final byte separately, and a resize
is what makes it likely, because the full-screen repaint it forces is exactly
when the next keystroke arrives. So the suite delivers a keypress that way --
introducer first, remainder after -- and also hammers whole keypresses sent with
no wait at all after a resize. Neither may stall the loop, and none of it waits
for a redraw before typing: a real user types immediately after dragging a
window edge.
"""
import os
import pathlib
import random
import sys
import time

from test_lvu_pty import PtyApp, isolated_environment, scratch_directory

SIZES = [(100, 30), (72, 20), (120, 40), (54, 16), (150, 38), (88, 24)]


def unsynchronized_clears(stream: bytes) -> int:
    """Whole-screen clears presented outside a DEC 2026 synchronized update.

    A bare `ESC [ 2 J` reaches the user as a blank screen for as long as the
    repaint takes to arrive, which a real terminal shows as a flash.
    """
    count = 0
    inside = False
    index = 0
    while index < len(stream):
        for marker, state in ((b"\x1b[?2026h", True), (b"\x1b[?2026l", False)):
            if stream.startswith(marker, index):
                inside = state
                index += len(marker)
                break
        else:
            if stream.startswith(b"\x1b[2J", index) and not inside:
                count += 1
            index += 1
    return count


def blocked_in_read(app: PtyApp) -> str:
    """Whether the child is asleep inside `read(2)`, for the failure report."""
    try:
        fields = (pathlib.Path("/proc") / str(app.process.pid) / "syscall").read_text().split()
    except OSError as error:
        return f"syscall unavailable ({error})"
    if fields and fields[0] == "0":
        return f"BLOCKED in read(2) on descriptor {fields[1]}: {' '.join(fields[:4])}"
    return f"syscall {fields[0] if fields else '?'}"


def responds(app: PtyApp, label: str) -> None:
    """The app must still open and close a dialog on ordinary keys."""
    app.send(b",")
    app.wait_until(lambda text: "Settings" in text,
                   f"{label}: Settings opens after the resize", timeout=6.0)
    app.send(b"\x1b")
    app.wait_until(lambda text: "Settings" not in text,
                   f"{label}: Settings closes after the resize", timeout=6.0)


def start(binary: pathlib.Path, root: pathlib.Path) -> PtyApp:
    source = root / "events.log"
    source.write_text("".join(f"LINE_{index:03d} hello world\n" for index in range(200)))
    app = PtyApp(binary, [str(source)], width=100, height=30,
                 environment=isolated_environment(root))
    app.wait_for("LINE_199", timeout=15.0)
    return app


def split_keypress_after_resize(binary: pathlib.Path) -> None:
    """The reported wedge: a resize, then a keypress delivered in two pieces."""
    with scratch_directory() as root:
        app = start(binary, root)
        try:
            before = len(app.transcript)
            app.resize(72, 20)
            # No wait for the resized frame, and no rescue key afterwards.
            app.send(b"\x1b[")
            time.sleep(0.05)
            app.send(b"B")
            try:
                app.wait_until(lambda text: len(app.transcript) > before,
                               "a frame after the split keypress", timeout=6.0)
                responds(app, "split keypress")
            except AssertionError as error:
                raise AssertionError(f"{error}\n--- child state: {blocked_in_read(app)}") from None

            # A truncated sequence must not wedge it either. The terminal never
            # sends the rest, so the read loop has nothing left to wake it.
            before = len(app.transcript)
            app.resize(120, 40)
            app.send(b"\x1b[")
            try:
                app.wait_until(lambda text: len(app.transcript) > before,
                               "a frame after a truncated sequence", timeout=6.0)
            except AssertionError as error:
                raise AssertionError(f"{error}\n--- child state: {blocked_in_read(app)}") from None
            # The stale introducer consumes the key that terminates it; the app
            # keeps running and the key after that is handled normally.
            app.send(b",")
            time.sleep(0.2)
            responds(app, "after a truncated sequence")

            app.send(b"q")
            assert app.wait_exit(timeout=8) == 0, "the app did not exit cleanly"
            app.assert_restored()
        finally:
            if app.process.poll() is None:
                app.process.kill()
                app.process.wait()
            app.close()


def whole_keypresses_after_resize(binary: pathlib.Path, cycles: int = 40) -> None:
    """Whole keypresses sent with no wait after a resize, over many sizes."""
    random.seed(20260907)
    with scratch_directory() as root:
        app = start(binary, root)
        try:
            for cycle in range(cycles):
                app.send(b",")
                app.wait_for("Settings", timeout=6.0)
                app.drain()
                before = len(app.transcript)
                app.resize(*random.choice(SIZES))
                app.send(b"\x1b")
                try:
                    app.wait_until(lambda text: "Settings" not in text,
                                   f"cycle {cycle}: Settings closes after the resize",
                                   timeout=6.0)
                except AssertionError as error:
                    raise AssertionError(f"{error}\n--- child state: {blocked_in_read(app)}") from None
                assert unsynchronized_clears(bytes(app.transcript[before:])) == 0, \
                    f"cycle {cycle}: the resize cleared the screen outside a synchronized update"
            app.send(b"q")
            assert app.wait_exit(timeout=8) == 0, "the app did not exit cleanly"
            app.assert_restored()
        finally:
            if app.process.poll() is None:
                app.process.kill()
                app.process.wait()
            app.close()


def main() -> None:
    binary = pathlib.Path(sys.argv[1]).resolve()
    split_keypress_after_resize(binary)
    whole_keypresses_after_resize(binary)
    print("Resize/keypress PTY passed: split and whole keypresses after a resize "
          "keep the loop running, the frame stays synchronized, and the terminal "
          "is restored")


if __name__ == "__main__":
    main()
