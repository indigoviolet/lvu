# lvu

A local terminal log workspace: capture once, progressively enrich with Polars,
save independent views, and investigate their data with local Paseo agents.

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
021 adds similar-source recipe suggestions in `r` (Alt-G refresh, Alt-A adapt,
`x` reject). AI adaptation currently changes only the advanced filter.
The command palette remains available: press Ctrl-P, type an operation, use Tab to
autocomplete, and Enter to execute. Operations show their shortcuts and availability.
Press `S` for storage usage and reviewed disposable-index cleanup.
Press `m` for reversible stack-trace grouping.
Press `t` for capture/event-time windows and rolling presets.
Press `r` for reusable named recipes. Use `n` then
`Ctrl-A` for source assistance, `A` for filter/enrichment proposals, and `I` for
resumable investigations. Existing sources continue capturing while views change.

To build the current source yourself:

```sh
mise run build:app
./target/debug/lvu-app --file /path/to/server.log
```

Checks include `mise run check:rust`, `mise run test:pty`, `mise run test:pty:real`,
`mise run check:expr`, and `mise run check:bridge`. Install the bridge's locked
Node dependencies first with `mise run install:bridge`. Expression and PTY tasks
manage their locked Python environments with uv through mise.
