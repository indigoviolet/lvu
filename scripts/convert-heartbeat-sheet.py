#!/usr/bin/env python3
"""Extract the supplied four-frame heartbeat sheet for Chafa conversion."""
import argparse
from pathlib import Path
from PIL import Image

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('source', type=Path)
parser.add_argument('output', type=Path)
args = parser.parse_args()
with Image.open(args.source) as sheet:
    if sheet.size != (887, 1774):
        parser.error('this layout expects the supplied 887x1774 four-frame sheet')
    if args.output.exists():
        parser.error('output already exists')
    # Fixed 320px squares, same horizontal anchor, containing each complete heart.
    frames = [sheet.convert('RGB').crop((282, center - 160, 602, center + 160))
              for center in (292, 658, 1034, 1401)]
    frames[0].save(args.output, save_all=True, append_images=frames[1:],
                   duration=[650, 100, 100, 150], loop=0, disposal=2)
