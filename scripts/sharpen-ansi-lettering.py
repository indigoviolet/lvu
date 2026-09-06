#!/usr/bin/env python3
"""Sharpen lower title lettering in half-block ANSI cells, leaving upper rows exact."""
import argparse
import math
import re
import shutil
import tomllib
from pathlib import Path

SGR = re.compile(r'(\x1b\[[0-9;]*m)')


def sharpen(text, width, height):
    lines = text.splitlines()
    pixels = [[(0, 0, 0)] * width for _ in range(height * 2)]
    for row, line in enumerate(lines):
        fg, bg, col = (255, 255, 255), (0, 0, 0), 0
        for token in SGR.split(line):
            if token.startswith('\x1b['):
                codes = list(map(int, token[2:-1].split(';')))
                i = 0
                while i < len(codes):
                    code = codes[i]
                    if code == 0:
                        fg, bg = (255, 255, 255), (0, 0, 0)
                    elif code == 40:
                        bg = (0, 0, 0)
                    elif code in (38, 48) and codes[i + 1] == 2:
                        rgb = tuple(codes[i + 2:i + 5])
                        if code == 38:
                            fg = rgb
                        else:
                            bg = rgb
                        i += 4
                    else:
                        raise ValueError('unsupported SGR')
                    i += 1
            else:
                for char in token:
                    if char not in ' ▀▄█':
                        raise ValueError('requires half-block art')
                    pixels[row * 2][col] = fg if char in '▀█' else bg
                    pixels[row * 2 + 1][col] = fg if char in '▄█' else bg
                    col += 1
        assert col == width
    # The supplied composition separates its heart and title at 55% of height.
    # Whole upper ANSI rows remain byte-for-byte identical, including colors.
    start = math.ceil(height * .55)
    def color(x, y):
        center = pixels[y][x]
        neighbors = [pixels[ny][nx] for ny in range(max(start * 2, y - 1), min(height * 2, y + 2))
                     for nx in range(max(0, x - 1), min(width, x + 2))]
        enhanced = tuple(max(0, min(255, round(c + 1.1 * (c - sum(n[k] for n in neighbors) / len(neighbors)))))
                         for k, c in enumerate(center))
        return (0, 0, 0) if max(enhanced) < 55 else enhanced
    for row in range(start, height):
        cells = []
        for x in range(width):
            top, bottom = color(x, row * 2), color(x, row * 2 + 1)
            cells.append('\x1b[38;2;' + ';'.join(map(str, top)) + ';48;2;' + ';'.join(map(str, bottom)) + 'm▀')
        lines[row] = ''.join(cells) + '\x1b[0m'
    return '\n'.join(lines) + '\n', start


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('source', type=Path)
    parser.add_argument('output', type=Path)
    args = parser.parse_args()
    data = tomllib.loads((args.source / 'animation.toml').read_text())
    args.output.mkdir(parents=True, exist_ok=False)
    shutil.copyfile(args.source / 'animation.toml', args.output / 'animation.toml')
    for entry in data['frames']:
        name = entry['file']
        assert Path(name).name == name
        original = (args.source / name).read_text()
        result, start = sharpen(original, data['width'], data['height'])
        assert result.splitlines()[:start] == original.splitlines()[:start]
        (args.output / name).write_text(result)
    with (args.output / 'animation.toml').open('a') as file:
        # Comment stays outside schema semantics even after [[frames]].
        file.write('\n# Lettering-only ANSI local contrast pass; upper heart rows unchanged.\n')
    print(f'{args.output}: sharpened title, upper {start} rows unchanged in every frame')


if __name__ == '__main__':
    main()
