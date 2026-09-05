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
025 adds global XDG settings, six themes, padded dialogs with visible input
cursors and shortcut footers, and highlighted discovery selection. Open settings
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
Press `t` for capture/event-time windows and rolling presets.
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
