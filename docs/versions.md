# Versions

lvu has one publication scheme: annotated `vX.Y.Z` tags and matching GitHub
releases. There is no separate preview channel or preview numbering. Every
version must agree with `crates/lvu-app/Cargo.toml` and `Cargo.lock`.

Current version: **[v0.1.5](https://github.com/indigoviolet/lvu/releases/tag/v0.1.5)**,
published 2026-09-10 at 01:51:54 UTC from `2a636c0`. All four native archives
passed release CI. The local installation is the official x86_64 Linux musl
archive, SHA-256 `1ad3dc004214776d6941e03cfef3e72804ca75d6aa28ec69d1ac632b5e2fcf11`.
Its bundled resources and eight actual terminal suites passed acceptance outside
the checkout, including named-zone save/restart and both role workflows;
`versions/latest` points to `v0.1.5`. What each version shipped is recorded in
[CHANGELOG.md](../CHANGELOG.md).

Exact-source acceptance passed 1392 Rust tests (7 ignored), clippy, bridge
checks and all 77 PTY suites. The volume-backed full soak remains unaccepted:
cold-query latency, autosave-flush shutdown and the broader 512 MB viewport
investigation remain unresolved. Live union/shared-key composition, enrichment
colour classification and shared background capture remain separate unreleased work.

See the [release runbook](release-runbook.md) for validation, archive publication,
Homebrew updates and installation checks. Archives contain the application,
Python expression helper and built bridge, so changing a development checkout
cannot change an installed release.

Local copies live under `versions/vX.Y.Z/` using the archive layout (`bin/lvu`
and `libexec/lvu/`). `versions/latest` names the current installed version;
`mise run lvu` launches it. The directory is ignored by Git. Retain a small
manifest of the source tag, source commit, checksums and acceptance evidence.
After installing and checking a new version, remove superseded local binaries
unless a running process still uses them. Published version tags remain immutable.

The earlier development-build numbering was retired on 2026-09-09. Old ledger
entries and build manifests are historical evidence, not another release scheme.
The compatibility table below retains those historical build identifiers because
renaming them would misrepresent when each schema change was introduced.

## Workspace schema compatibility

The workspace database under `<capture-root>/workspace/` carries a schema
version. A build refuses to open a database whose version is newer than its
own (`crates/lvu-memory/src/store.rs`), so downgrading past one of these
lines needs the database set aside first (see `AGENTS.md`, Coordination).

| Schema | First historical build | What it added |
| --- | --- | --- |
| v3 | 032 | Explicit source membership for merged views. |
| v4 | 034 | Durable command-enrichment attempts and results. |
| v5 | 048 | Views gained a role so every source has a permanent All events view; existing views keep their definition. |
| v6 | 048 | Bookmarks became source-scoped; notes held by several views for one record are joined, never dropped. |

Additive changes since v6 (recipe revision dates, the session record beside
the database, per-view command steps) are `serde(default)` fields or plain
files and need no bump: an older build ignores them.
