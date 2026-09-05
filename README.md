# lvu

A local terminal log workspace: capture once, progressively enrich with Polars,
save independent views, and investigate their data with local Paseo agents.

Implementation is starting. See [the implementation plan](docs/implementation-plan.md)
for product scope, architecture, acceptance criteria, and work sequencing, and
[the work ledger](docs/work-ledger.md) for actual progress. Planned features are
not claims of working functionality.

Development tools and tasks are managed by mise:

```sh
mise trust
mise install
mise run doctor
```

Rust check tasks are defined for the incoming workspace; they become runnable
after its implementation is integrated. Additional component tasks will be added
with their reviewed implementations.
