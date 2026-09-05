# lvu-memory

`lvu-memory` owns editable named source/view recipes and autosaved workspace
metadata. It does not own capture bytes, cache eviction, UI state machines, or AI.

## Layout and authority

An application-owned workspace directory contains `workspace.sqlite3` and a
`recipes/` directory. Recipe paths are derived only from `RecipeId` UUIDs, never
from user-visible names. TOML is authoritative for explicit recipe saves/imports;
SQLite is authoritative for working view drafts, applied state, navigation,
source observations, usage, and suggestion outcomes.

An explicit recipe save takes the recipe-directory publication lock, validates
the immutable revision against SQLite, starts an immediate transaction, compares
the caller's last-seen file hash, writes a create-new temporary file, fsyncs it,
atomically renames it, and fsyncs the directory before committing SQLite. A crash
between those stores is reconciled from TOML on the next open. The same lock covers
reconciliation, so concurrent writers cannot reorder the file and database
pointers. A revision UUID can never acquire different bytes or a different hash.
Unknown/future TOML and databases return readable errors and are not reset.

External imports are installed into the UUID-addressed canonical recipe path via
that same save lifecycle; they are not remembered as mutable external references.

SQLite uses foreign keys, immediate write transactions where multi-row atomicity
is required, a bounded busy timeout, and compare-and-swap view versions. The
library never deletes capture data or follows user names as filesystem paths.
Recent/fingerprint/suggestion lookups have supporting indexes. Candidate return
counts and the in-process scoring pool are capped, but SQLite may still inspect
additional indexed rows for ordering and correlated outcome aggregation; scaling
that query further is an integration follow-up, not an embedding/model feature.

## View constraints and environment values

The default view constraint is literal case-insensitive text search. Empty search
is unconstrained. An optional advanced Python Polars filter is stored separately;
query execution combines it with search using logical AND. Applied search/filter
and unfinished drafts/diagnostics are distinct SQLite columns.

The embedded `SourceDefinition` is the accepted `lvu-core` type. Command
environment entries are serialized only when they are explicitly present in that
definition; this crate never reads or expands the ambient process environment.
Callers should pass references through their source configuration when values
must not be stored literally. No key-name guessing or interactive secret policy is
performed here.

## Root mise task proposal

After workspace integration, the primary can add a named TOML recipe equivalent
to:

```toml
[tasks."check:memory"]
description = "Format, test, and lint persistent memory"
run = [
  "cargo fmt --manifest-path crates/lvu-memory/Cargo.toml --all -- --check",
  "cargo test --manifest-path crates/lvu-memory/Cargo.toml --locked",
  "cargo clippy --manifest-path crates/lvu-memory/Cargo.toml --all-targets --locked -- -D warnings",
]
```
