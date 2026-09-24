# Versions

lvu has one publication scheme: annotated `vX.Y.Z` tags and matching GitHub
releases. There is no separate preview channel or preview numbering. Every
version must agree with `crates/lvu-app/Cargo.toml` and `Cargo.lock`.

Candidate **v0.1.18** fixes automatic setup, assistance ownership and quit/input
lifecycle. It is undergoing exact-source acceptance and is not published yet;
the current installed/public release below remains authoritative.

Current version: **[v0.1.17](https://github.com/indigoviolet/lvu/releases/tag/v0.1.17)**,
published 2026-09-19 at 06:27:32 UTC from `f665d25`. Release CI built and
executed the three supported native archives. The local installation is the
official x86_64 Linux musl archive, SHA-256
`6e4e4b77adf19b64a8d5e07cd4c972d00b5a9f3c3f19c61f25a7c232da194b11`;
its installed binary is `2c356e058c9910e8e0c55cccf2f2d9a5db885232c3d03cda62f8ab547fef7210`.
Bundled helper/bridge execution and the Docker service/container terminal suite
passed outside the checkout; `versions/latest` points to `v0.1.17`.

Exact-source acceptance passed the full Rust workspace, all-target Clippy, bridge
checks, all 90 PTY suites and the source-bound shared-capture/remote-union
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
