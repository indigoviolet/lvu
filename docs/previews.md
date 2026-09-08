# Development previews

A preview is a locally published, immutable copy of the application built
from a named main commit. Each lives under `previews/<number>-<name>/` with
the binary and a `manifest.json` recording the source revision, checksum,
the checks that were run and known limitations; `previews/latest` points at
the newest tested one and `mise run preview` runs it. The directory is
gitignored: previews are build artifacts, not source.

## Tags required for future previews

From 2026-09-08 onward, every checkpoint selected for a preview-worthy release
must receive an immutable annotated Git tag on the exact validated source
commit. Development previews use `preview-<number>`, matching their directory
number; the next is `preview-058`. Versioned releases continue to use `vX.Y.Z`
and the [release runbook](release-runbook.md).

After building the copied preview binary and completing its acceptance checks:

1. Record the exact source commit, binary SHA-256, checks actually run, and
   known limitations in the preview manifest. Add `source_tag`, for example
   `preview-058`. The source checkout must be clean when the binary is built.
2. Create an annotated tag on that source commit. Include the preview name,
   binary checksum and acceptance evidence in the annotation. Verify that
   the tag resolves to the manifest's `source_commit`; tagging a later
   documentation commit would identify different source.
3. Push only that tag to origin and verify the remote tag resolves to the same
   commit. A tag that already exists must be checked, never force-moved or
   reused for different bytes. If publication needs a corrected build, use a
   new preview number and tag.
4. Record the tag, revision and acceptance results in the work ledger, then
   advance `previews/latest` to the tested directory. Tag publication is a
   required part of this process.

The `preview-*` namespace does not match the current release workflow's `v*`
trigger. Publishing a preview tag records the source without starting the
versioned archive release workflow. Existing previews remain immutable; this
requirement applies prospectively and does not retag historical artifacts.

Where to look for what a preview contains:

- `previews/<n>/manifest.json` on the machine that published it.
- `TODO.md`, "Shipped since preview 047": the Done rows by the preview that
  first carried them, with the publication commit.
- `docs/work-ledger.md` for the acceptance evidence behind each publication.
- The narrative history of previews 001 to 043 (source commit, checksum,
  acceptance, limitations) is in this file's history at `14ad91c`.

## Workspace schema compatibility

The workspace database under `<capture-root>/workspace/` carries a schema
version. A build refuses to open a database whose version is newer than its
own (`crates/lvu-memory/src/store.rs`), so downgrading past one of these
lines needs the database set aside first (see `AGENTS.md`, Coordination).

| Schema | First preview | What it added |
| --- | --- | --- |
| v3 | 032 | Explicit source membership for merged views. |
| v4 | 034 | Durable command-enrichment attempts and results. |
| v5 | 048 | Views gained a role so every source has a permanent All events view; existing views keep their definition. |
| v6 | 048 | Bookmarks became source-scoped; notes held by several views for one record are joined, never dropped. |

Additive changes since v6 (recipe revision dates, the session record beside
the database, per-view command steps) are `serde(default)` fields or plain
files and need no bump: an older build ignores them.
