# User-supplied animated title

`source.gif` is the supplied artwork, preserved unchanged. Chafa 1.18.2 converts
its composited frames to true-color ANSI half-block characters. Both sizes retain
all 10 frames at 110 ms each: a repeating 1,100 ms cycle. No timing is encoded in
ANSI itself; `animation.toml` records it.

## Preview

From the repository root:

```sh
mise install http:chafa
mise run art:preview        # 80 columns × 22 rows minimum
mise run art:preview:large  # 120 columns × 40 rows minimum
```

Escape, q or Ctrl-C exits. Playback restores terminal attributes and the normal
screen. Each directory also contains `ansi-preview.gif`, reconstructed from the
actual colored character cells for convenient visual review, and a still PNG.
These assets are not yet wired into lvu's startup renderer.

The original aspect ratio and complete canvas are retained, with black padding
inside the declared terminal dimensions. At a typical 1:2 terminal font ratio,
Chafa's image occupies 59×22 and 107×40 cells respectively. The larger version
keeps more letter detail. No dithering or extra punctuation glyphs are used.

## Regenerate or convert another animation

```sh
mise exec -- uv run --with pillow==11.3.0 --no-project python \
  scripts/convert-ansi-animation.py input.gif new-output-directory --size 80x22
mise exec -- python scripts/play-ansi-animation.py new-output-directory/animation.toml
```

The output directory must not exist. Pillow composites GIF disposal frames and
extracts durations; Chafa converts each resulting PNG. Cursor visibility controls
from Chafa are removed; generated frames contain only SGR colors, half-block glyphs
and complete padded rows. Missing/zero durations fail rather than inventing timing.
The manifest includes a source checksum, Chafa version, dimensions, loop metadata,
rest frame and ordered file/duration entries. GIF loop count 0 means infinite;
absence of loop metadata plays once. Positive counts mean repeat that many times.

The mise Chafa download is pinned by URL and SHA-256 for the current Linux x86-64
development host. Other platforms need their own supported Chafa installation.
Chafa is an asset conversion dependency; generated frames can be played without it.
See the [Chafa manual](https://hpjansson.org/chafa/man/) for conversion options.

## Activity indicator artwork

The same converter can create a separate activity animation. Use a fixed canvas
with padding for the largest pulse and one unambiguous resting frame. For an
8-column × 1-row indicator, a 16×4-pixel source is a practical starting point;
it reduces to only 8×2 colored half-cell pixels. A 16×8-pixel source targeting
8 columns × 2 rows gives substantially more heart detail, but needs a taller footer.
A higher-resolution source of the same aspect ratio is also fine. Keep timing in
the GIF, use transparent or black background, and avoid lettering/tiny highlights.
Current conversion composites transparency onto black. Theme-aware transparent
backgrounds and binding playback to actual application activity need UI integration.

## Sharper lettering variant

The preview tasks now select `80x22-sharp` and `120x40-sharp`. These apply local
contrast sharpening only to lower ANSI lettering rows, after Chafa conversion.
The heart's upper 13/22 rows remain byte-for-byte identical in all ten frames;
source GIF, canvas size and 110 ms frame timing remain unchanged. The original
`80x22`/`120x40` conversions remain available for comparison. Sharpening improves
edge contrast but cannot recover fine lettering detail lost at terminal resolution.

To reproduce from an original conversion:

```sh
mise exec -- python scripts/sharpen-ansi-lettering.py \
  assets/startup/love-you-log-time/120x40 new-sharp-directory
```

The 55% lettering boundary is specific to this supplied composition, not a general
text detector. This script edits ANSI color cells, not the original GIF artwork.

Background colors use explicit RGB black for both the artwork and padding. The
preview clears its surrounding terminal canvas with the same RGB black; ANSI
palette black is intentionally avoided because terminal themes may render it gray.
