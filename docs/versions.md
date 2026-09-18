# Versions

lvu has one publication scheme: annotated `vX.Y.Z` tags and matching GitHub
releases. There is no separate preview channel or preview numbering. Every
version must agree with `crates/lvu-app/Cargo.toml` and `Cargo.lock`.

Current version: **[v0.1.15](https://github.com/indigoviolet/lvu/releases/tag/v0.1.15)**,
published 2026-09-18 at 05:08:20 UTC from `2489b87`. Release CI built and
executed the three supported native archives. The local installation is the
official x86_64 Linux musl archive, SHA-256
`27b32a9ffc51147a9a9752bdcbdb2285232230db9530c71f8a336c3d2e25dc0c`;
its installed binary is `28b30b073697ba2ba425d9b640a5c020f2d3a5a862cdf6a7c48c9cef94561ed5`.
Bundled helper/bridge execution and the Docker service/container terminal suite
passed outside the checkout; `versions/latest` points to `v0.1.15`.

Exact-source acceptance passed the full Rust workspace, all-target Clippy, 120
bridge tests, all 88 PTY suites and ten source-bound shared-capture/remote-union
proofs. Human Apple-silicon macOS and arm64 Linux terminal acceptance remains
unverified. The volume-backed full soak, cold-query latency and slow-storage
autosave investigations remain paused and unaccepted. What each version shipped
is recorded in [CHANGELOG.md](../CHANGELOG.md).

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
