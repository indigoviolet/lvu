# lvu

A local terminal log workspace: capture once, progressively enrich with Polars,
save independent views, and investigate their data with local agents.

Implementation is in progress. See [the implementation plan](docs/implementation-plan.md)
for product scope, architecture, acceptance criteria, and work sequencing, and
[the work ledger](docs/work-ledger.md) for actual progress. Planned features are
not claims of working functionality.

Development tools and tasks are managed by mise:

```sh
mise trust
mise install
mise run doctor
```

Run the current local preview with `mise run preview`; see
[preview notes](docs/previews.md) for supported behavior and limitations. Preview
026 adds iterative regex/Polars enrichment, richer debounced searches, gzip files,
horizontal event scrolling, and a shaded heart above gold title lettering. It retains
global XDG settings, six themes, padded dialogs and visible input cursors. Open settings
with `,`. Preferences live in `$XDG_CONFIG_HOME/lvu/settings.toml` (normally
`~/.config/lvu/settings.toml`). Cache limits are saved there and take effect after
restart; durable captures are separate from disposable indexes.

Source-less startup shows the pixel-art **LOVE YOU LOG TIME** title until Escape.
CLI files, commands and redirected stdin skip the title. `LVU_REDUCED_MOTION=1`
keeps the heart static; `LVU_NO_DELIGHT=1` disables the accents.
Use Ctrl-P to find operations and their shortcuts. Recipe suggestions remain
in `r` (Alt-G refresh, Alt-A adapt, `x` reject).
Press `S` for storage usage and reviewed disposable-index cleanup.
Press `m` for reversible stack-trace grouping.
Press `t` for capture/event-time windows and rolling presets. `Alt-T` inside that
dialog prepares a reviewed timestamp-enrichment prompt.
Press `r` for reusable named recipes. Use `n` then
`Ctrl-A` for source assistance, `A` for filter/enrichment proposals, and `I` for
resumable investigations. Existing sources continue capturing while views change.


```sh
mise run preview app.log worker.log
mise run preview --command 'docker logs -f api'
producer | mise run preview
```

Use `mise run preview --help` for source options. The task preserves quoted
command arguments and file paths, and passes redirected input to the viewer.

Stdin capture currently supports Linux pipes, regular-file redirection and /dev/null.
It needs an interactive controlling terminal; EOF leaves captured logs open.

To build the current source yourself:

```sh
mise run build:app
./target/debug/lvu-app --file /path/to/server.log
```

Checks include `mise run check:rust`, `mise run test:pty`, `mise run test:pty:real`,
`mise run check:expr`, and `mise run check:bridge`. Install the bridge's locked
Node dependencies first with `mise run install:bridge`. Expression and PTY tasks
manage their locked Python environments with uv through mise.

Search (`/`) accepts literal raw text, `level: error`, `/timeout|refused/i`,
`message: /^timeout/`, or a `pl.col(...)` Boolean expression. Field selectors use
colon followed by a space. Literal text is case-insensitive; regex flags are
`i`, `m`, and `s`. Searches debounce for 300 ms; clearing restores unconstrained
search while retaining other view constraints.

Enrichment (`e`) appends successful stages. For example:
`/request=(?P<request_id>\S+) status=(?P<status>\d+)/` adds both fields.
Then add `upper_id = pl.col('request_id').str.to_uppercase()` without losing them.
Use Alt-A to add, Alt-E to edit, and Alt-R to remove the selected stage. Invalid
changes retain the last good chain. Recipes and working state retain stage IDs
and exact editable definitions.

Left/Right scroll event text by eight terminal columns; `0` resets it. Metadata
columns stay fixed. Gzip files are detected by content and read as static archives;
unchanged reopen avoids duplicates. Changed archives require a fresh capture identity.
