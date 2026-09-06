#!/usr/bin/env python3
"""Convert composited GIF frames with Chafa; preserve their original durations."""
import argparse
import hashlib
import json
import re
import subprocess
import tempfile
from pathlib import Path

from PIL import Image

SGR = re.compile(r"\x1b\[[0-9;]*m")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("output", type=Path, help="New directory (existing output is never overwritten)")
    parser.add_argument("--size", default="80x22", help="Maximum terminal columns x rows")
    args = parser.parse_args()
    width, height = map(int, args.size.split("x"))
    if not 1 <= width <= 240 or not 1 <= height <= 100:
        parser.error("size must be within 240x100 cells")
    version = subprocess.check_output(["chafa", "--version"], text=True).splitlines()[0]
    args.output.mkdir(parents=True, exist_ok=False)
    with Image.open(args.source) as gif, tempfile.TemporaryDirectory(prefix="lvu-chafa-") as temporary:
        if gif.n_frames > 1000:
            parser.error("at most 1000 frames are supported")
        manifest = [
            "schema_version = 1",
            f"generator = {json.dumps(version)}",
            f"source_sha256 = {json.dumps(hashlib.sha256(args.source.read_bytes()).hexdigest())}",
            f"width = {width}", f"height = {height}",
            f"loop = {str(gif.info.get('loop') == 0).lower()}",
            f"gif_loop_count = {gif.info.get('loop', -1)}",
            'reduced_motion_frame = "frame-001.ans"',
        ]
        durations = []
        for index in range(gif.n_frames):
            # Sequential seek applies GIF disposal/compositing before RGB conversion.
            gif.seek(index)
            duration = gif.info.get("duration", 0)
            if duration <= 0:
                raise ValueError(f"frame {index} has no positive duration; choose a timing policy explicitly")
            durations.append(duration)
            png = Path(temporary) / "frame.png"
            gif.convert("RGBA").save(png)
            ansi = subprocess.check_output([
                "chafa", "--format=symbols", "--colors=full", "--symbols=vhalf",
                "--size=" + args.size, "--view-size=" + args.size,
                "--font-ratio=1/2", "--animate=off", "--probe=off",
                "--optimize=0", "--relative=off", "--bg=000000",
                "--threads=2", str(png),
            ], text=True)
            ansi = ansi.replace("\x1b[?25l", "").replace("\x1b[?25h", "")
            lines = ansi.rstrip("\n").split("\n")
            if len(lines) > height:
                raise ValueError("Chafa exceeded requested height")
            padded = []
            for line in lines:
                plain = SGR.sub("", line)
                if any(c not in " ▀▄█" for c in plain) or len(plain) > width:
                    raise ValueError("unexpected Chafa control sequence, glyph or width")
                left = (width - len(plain)) // 2
                padded.append("\x1b[0;48;2;0;0;0m" + " " * left + line + "\x1b[0;48;2;0;0;0m" + " " * (width - left - len(plain)) + "\x1b[0m")
            blank = "\x1b[0;48;2;0;0;0m" + " " * width + "\x1b[0m"
            top = (height - len(padded)) // 2
            padded = [blank] * top + padded + [blank] * (height - top - len(padded))
            name = f"frame-{index + 1:03}.ans"
            (args.output / name).write_text("\n".join(padded) + "\n")
            manifest += ["", "[[frames]]", f"file = {json.dumps(name)}", f"duration_ms = {duration}"]
        (args.output / "animation.toml").write_text("\n".join(manifest) + "\n")
        print(f"{args.output}: {len(durations)} frames, {sum(durations)} ms, {width}x{height} cells")


if __name__ == "__main__":
    main()
