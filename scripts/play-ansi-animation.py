#!/usr/bin/env python3
"""Preview generated ANSI frames with manifest timing; Escape/Ctrl-C exits."""
import argparse
import os
import re
import select
import shutil
import sys
import termios
import time
import tomllib
import tty
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("manifest", type=Path)
    parser.add_argument("--once", action="store_true")
    args = parser.parse_args()
    data = tomllib.loads(args.manifest.read_text())
    width, height = data["width"], data["height"]
    frames = []
    for entry in data["frames"]:
        name = entry["file"]
        if Path(name).name != name:
            parser.error("frame filenames must be local basenames")
        frame = (args.manifest.parent / name).read_text()
        plain = re.sub(r"\x1b\[[0-9;]*m", "", frame)
        lines = plain.splitlines()
        if len(lines) != height or any(len(line) != width for line in lines) or any(c not in " ▀▄█\n" for c in plain):
            parser.error("frame must contain the declared grid and SGR colors only")
        if not 1 <= entry["duration_ms"] <= 60000:
            parser.error("invalid frame duration")
        frames.append((frame.splitlines(), entry["duration_ms"] / 1000))
    if not frames or not sys.stdin.isatty() or not sys.stdout.isatty():
        parser.error("playback needs an interactive terminal and nonempty animation")
    if shutil.get_terminal_size().columns < width or shutil.get_terminal_size().lines < height:
        parser.error(f"requires a terminal at least {width}x{height}")
    fd = sys.stdin.fileno()
    previous = termios.tcgetattr(fd)
    try:
        tty.setcbreak(fd)
        sys.stdout.write("\x1b[?1049h\x1b[?25l\x1b[?7l\x1b[2J")
        loops = 1 if args.once else (None if data.get("loop") else max(1, data.get("gif_loop_count", 0) + 1))
        cycle = 0
        while loops is None or cycle < loops:
            for lines, duration in frames:
                size = shutil.get_terminal_size()
                left = max(0, (size.columns - width) // 2)
                top = max(0, (size.lines - height) // 2)
                sys.stdout.write("\x1b[?2026h\x1b[2J")
                for row, line in enumerate(lines[:size.lines]):
                    sys.stdout.write(f"\x1b[{top + row + 1};{left + 1}H" + line)
                sys.stdout.write("\x1b[?2026l")
                sys.stdout.flush()
                deadline = time.monotonic() + duration
                while (remaining := deadline - time.monotonic()) > 0:
                    if select.select([fd], [], [], remaining)[0] and os.read(fd, 32).startswith((b"\x1b", b"q")):
                        return
            cycle += 1
    except KeyboardInterrupt:
        pass
    finally:
        sys.stdout.write("\x1b[?2026l\x1b[0m\x1b[?7h\x1b[?25h\x1b[?1049l")
        sys.stdout.flush()
        termios.tcsetattr(fd, termios.TCSADRAIN, previous)


if __name__ == "__main__":
    main()
