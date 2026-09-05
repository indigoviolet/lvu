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
[preview notes](docs/previews.md) for supported behavior and limitations. The first
preview uses synthetic logs. Real acquisition and query adapters are still being
integrated.

To build the current source yourself:

```sh
mise run build:tui
./target/debug/lvu --demo
```

Checks include `mise run check:rust`, `mise run test:pty`,
`mise run check:expr`, and `mise run check:bridge`. Install the bridge's locked
Node dependencies first with `mise run install:bridge`. Expression and PTY tasks
manage their locked Python environments with uv through mise.
