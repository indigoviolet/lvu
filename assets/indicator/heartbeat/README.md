# Corner heartbeat

The supplied `source-sheet.png` contains four vertical frames: resting heart,
trace entering, peak, trace leaving. `source.gif` extracts equal 320x320 canvases
and assigns 650/100/100/150 ms durations (a one-second cycle). Timing was chosen
for this still sprite sheet; it was not embedded in the original PNG.

`14x7/` contains the Chafa 1.18.2 true-color half-block conversion and timing
manifest. Smaller 8x3/8x4 studies lost the pulse shape and were not selected.
The application reserves the lower-left selector corner at terminals >=80x24,
without reducing log row capacity. The main status strip remains one row.
Near-black surround pixels become the current theme background so the sprite
has no black rectangular border on a light/dark theme.

Active/pending work animates. Idle, errors and reduced-motion use the resting
frame; ordinary idle/working labels are omitted. Errors and measured progress can
still be reported. Narrow/short terminals and ASCII mode use the compact badge.
Dialogs cover the corner normally; it cannot paint over them or intercept input.

Reproduce from the repository root, using new output paths:

```sh
mise exec -- uv run --with pillow==11.3.0 --no-project python \
  scripts/convert-heartbeat-sheet.py assets/indicator/heartbeat/source-sheet.png new-heart.gif
mise exec -- uv run --with pillow==11.3.0 --no-project python \
  scripts/convert-ansi-animation.py new-heart.gif new-heart-ansi --size 14x7
```

Application rendering decodes the checked-in SGR colors into Ratatui cells once;
no Chafa/Python subprocess or direct ANSI writes run in the UI tick.
