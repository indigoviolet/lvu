# Changelog

User-facing changes per version. Only `vX.Y.Z` versions are releases; there is
no preview channel. Unfinished and proposed work lives in [TODO.md](TODO.md);
validation evidence lives in the [work ledger](docs/work-ledger.md).

## v0.1.5 — 2026-09-10

- Settings accepts IANA display zones such as `Europe/Berlin`, with the offset
  determined for each instant, including daylight-saving transitions. UTC and
  fixed offsets remain available. Timestamps show explicit offsets and dates
  use the same zone; captured bytes and accepted event instants are unchanged.
- Invalid custom zone drafts retain the last valid preview. Stale save
  completions preserve newer drafts, and loaded zones apply at startup.

[Published release](https://github.com/indigoviolet/lvu/releases/tag/v0.1.5),
from `2a636c0`, with all four native archives. The installed Linux archive
passed eight mandatory PTY suites, including named-zone save and restart.

## v0.1.4 — 2026-09-10

[Published release](https://github.com/indigoviolet/lvu/releases/tag/v0.1.4),
from `984b6f0`, with all four native archives. The installed Linux archive
passed all eight mandatory PTY suites, including both role workflows.

- Severity and timestamp roles use reviewed enrichment columns. Fields can assign
  either role; assistance can propose a severity or UTC timestamp column through
  the ordinary enrichment review flow. Canonical ready severity values supply
  styling. Timestamp display uses the accepted time basis and configured zone;
  pending or invalid derived values show a placeholder. Roles persist across
  restart. Normal row projection no longer guesses severity or event timestamps.

## v0.1.3 — 2026-09-09

[Published release](https://github.com/indigoviolet/lvu/releases/tag/v0.1.3),
from `8ca75d0`, with all four native archives and the updated Homebrew tap.

### Features

- Source assistance can propose up to eight distinct sources together. Review
  every item before Apply; results name partial failures and retain an
  all-failed review for retry. Existing single-source proposals remain valid.
  Admission compares acquisition identities and policies against live and
  pending sources to avoid duplicate starts or silently changed settings.

- Grouping (`m` or `z`) offers Run, Filter and Off over enrichment columns.
  Run collapses consecutive equal keys. Filter starts an event at each non-null
  value and joins following null-valued records until the next start. Define
  extraction patterns in Enrichment. Long events retain their full membership
  with a bounded display page and explicit shown/total counts. Bytes, identities
  and filter membership are unchanged.

### Fixes

- Page reads and capture batches take turns without occupying each other’s
  queue slots. Writer closure and overdue commits retain priority; fallback
  reads stay bounded even when their callers cancel.

- Batch queued view saves into one transaction, with per-view rollback and
  acknowledgements only after durable completion. Shutdown deadlines are
  unchanged; the full slow-volume soak remains unresolved.

- Missing enrichment or filter columns report which input is unavailable in
  the current batch, including later-stage ordering mistakes. Rejected drafts
  preserve the last accepted view and live refresh.

- Keep the drawable beginning of a requested log window when its rows exceed
  the display cache byte limit. Show loading while indexed rows are not yet
  drawable. This addresses one blank-viewport case, not the full capture soak.

- Status line prioritizes actionable notices, event-time diagnostics, row
  ranges and visible return/help controls; wide and combining text fits, with
  optional indicators giving way as whole segments.
- FOLLOW draws the newest servable window while preserving true-tail
  selection. HISTORY keeps its requested destination separate from retained
  displayed rows, retries without another keypress, and reports the actual
  range.
- Pending raw-context location stays visible in narrow status lines: the
  protected prefix says `locating #N` until the chase resolves or reports its
  bounded failure.

### Breaking changes

- Normal folding controls now open the unified Grouping dialog. New rules use
  enrichment columns; saved legacy rules retain their meaning until changed.

## [v0.1.2](https://github.com/indigoviolet/lvu/releases/tag/v0.1.2) — 2026-09-09

### Features

- View Summary lists the applied sources, time basis, enrichment, filters,
  grouping, folding, columns, colour rules and readiness; Enter opens the
  owning dialog.
- Ordered predicate colour rules per view, with regex span highlighting of
  matches. A rejected candidate leaves the last valid applied view intact.
- The Fields Value pane shows its 2,048-record sample immediately, counts the
  rest, then reports exact whole-view present/distinct counts, top values and
  range, including nested JSON paths. The pass reads only the asked column and
  is bounded and cancellable (620k records in 0.90 s, 3M in 4.1 s).
- Faster scans: merged-view order extends per refresh instead of rebuilding,
  and ASCII-insensitive regex takes a faster path on ASCII rows with exact
  Unicode semantics kept for the rest.
- `X` stops and `R` restarts the selected source from Logs or the sidebar;
  Help and the palette show both keys.
- More useful bounded assistance: source-specific discovery context, one
  wider sample with per-answer coverage compared against the prior answer in
  the existing proposal pane, and concurrent settlement of assistance paths
  at shutdown.

### Fixes

- ANSI control sequences are hidden in logs, pinned values, folded patterns
  and Details; captured and queried bytes are unchanged.
- The `Extracted` time basis reads `timestamp_utc` through the same native
  Polars expression in live views and exports, with checked whitespace, UTC
  aliases, offsets, fractions and timestamp limits.
- Bridge startup failures finish cleanly: staged input is released, pending
  requests get the concrete diagnostic, and the bridge exits instead of
  keeping Node alive. Normal live-input backpressure is unchanged.

### Breaking changes

- No new breaking changes recorded.

### Known limitations

- The volume-backed full soak has not passed: a 12.921 s first cold query
  and memory autosave-flush shutdown failures remain unresolved. No
  general capture-throughput, full-soak or retained-memory claim is made.
- The arm64 Linux and both macOS archives are built and executed by CI but
  have had no human use. macOS archives are unsigned and unnotarized.
  Windows is unsupported and not packaged.

## [v0.1.1](https://github.com/indigoviolet/lvu/releases/tag/v0.1.1) — 2026-09-08

### Features

- Filtering is about fifteen times faster on large captures (620k records in
  0.71–0.81 s, previously ~11 s); capture commits in groups instead of
  tracking the scheduler (66 s → 39 s settle, 0.9 s → 0.1 s shutdown).
- Merged multi-source views interleave by event, extracted or chosen-column
  time, keeping source order on the capture basis; the order row says which.
- `o` jumps to the selected record in its source's All events view and back,
  replacing the raw-context dialog.
- Search and Advanced are two tabs of one Filter dialog; both constraints
  apply together and the title names every applied constraint.
- Folding by any column with a per-view Folding dialog, fold/unfold from
  Fields, and no more blank panes on large captures.
- Nested JSON renders as an in-place tree with a Fields Value pane (type,
  counts, top values, range, one-key Pin/Filter/Exclude/Color/Fold/Correlate).
- Underlined dialog letters press their buttons; `lvu --keys` shows what a
  terminal actually sends.
- Sixteen-colour terminals get readable identity colours and chrome.
- New arm64 Linux archive, built and executed natively by CI.

### Fixes

- Quitting while a short-lived command source was finishing no longer exits
  non-zero.

### Breaking changes

- Bare `lvu` now resumes the last session by default: files continue from
  their durable cursor, commands re-run with their recorded directory and
  environment, and named arguments add to that set. `--fresh` starts with
  nothing acquired (deleting nothing); `--resume` spells the default
  explicitly.
- The `p` key is retired: `/` opens the Filter dialog on Search and Alt-A
  reaches the Advanced tab.

## [v0.1.0](https://github.com/indigoviolet/lvu/releases/tag/v0.1.0) — 2026-09-08

Initial public release: a live local log viewer for files, commands and
stdin, with literal, `/regex/` and Polars-expression search, stepped
enrichment, independent and merged views, recipes, bookmarks, export, and
optional assistance through a local bridge.

### Features

- Capture from files, commands and stdin into a durable local store; captured
  bytes and record identities are never changed by viewing.
- Permanent per-source All events views; filters, enrichment, grouping,
  folding, colouring and time windows apply per view with last-good rollback.
- Saved, revisable and exportable recipes; reviewed external-command
  enrichment with durable attempts.
- Optional assistance through a local bridge. Advanced Polars expressions
  need `uv` on `PATH`; assistance needs `node` on `PATH` plus an installed
  and authenticated agent CLI.

### Fixes

- None; initial release.

### Breaking changes

- None; initial release with no prior version to upgrade from. The workspace
  schema is v6.
