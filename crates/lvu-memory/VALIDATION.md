# M5A validation

Validated with repository-managed Rust 1.98.1:

```text
mise exec rust@1.98.1 -- cargo fmt --manifest-path crates/lvu-memory/Cargo.toml --all -- --check
mise exec rust@1.98.1 -- cargo test --manifest-path crates/lvu-memory/Cargo.toml --locked
mise exec rust@1.98.1 -- cargo clippy --manifest-path crates/lvu-memory/Cargo.toml --all-targets --locked -- -D warnings
```

The 19 tests use real temporary TOML files and bundled SQLite databases. They cover
exact multiline expression round trips, literal search semantics, protected
outputs, compare-and-swap saves and views, independent drafts/applied state,
independent navigation, immutable history/undo, startup reconciliation, stable
source identities and missing files, bounded cursor pages, deterministic candidate
evidence and suggestion outcomes, future versions, malformed/truncated input,
and corruption without silent reset.
Review regressions additionally cover immutable-revision preflight before file
publication, stale concurrent recipe writers, canonical-first interruption
reconciliation, insert-only recipe source snapshots, bounded growing-file reads,
future-version overwrite refusal, canonical external imports, and applied/draft
working-view size validation.
