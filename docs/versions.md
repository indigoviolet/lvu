# Versions

lvu has one publication scheme: annotated `vX.Y.Z` tags and matching GitHub
releases. There is no separate preview channel or preview numbering. Every
version must agree with `crates/lvu-app/Cargo.toml` and `Cargo.lock`.

Current version: **[v0.1.6](https://github.com/indigoviolet/lvu/releases/tag/v0.1.6)**,
published 2026-09-10 at 05:44:57 UTC from `697865e`. All four native archives
passed release CI. The local installation is the official x86_64 Linux musl
archive, SHA-256 `efc28fe5a2046d98997a9e9531761719dec0df579d7d78674422282f96baad59`.
Its bundled resources and eleven actual terminal suites passed acceptance outside
the checkout, including ordinary, shared-key and colour union coverage;
`versions/latest` points to `v0.1.6`. What each version shipped is recorded in
[CHANGELOG.md](../CHANGELOG.md).

Exact-source acceptance passed 1510 Rust tests (7 ignored), clippy, 105 bridge
tests and all 80 PTY suites. Live unions, enrichment shared keys and exact-value
colour classification are published, including creation-time key pinning and
persistent manual pin changes. Shared background capture remains unreleased.
The volume-backed full soak remains unaccepted: cold-query latency,
autosave-flush shutdown and the broader 512 MB viewport investigation remain
unresolved and long-running validation is paused.

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
