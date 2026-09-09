# Changelog

User-facing changes per version. Only `vX.Y.Z` versions are releases; there is
no preview channel. Unfinished and proposed work lives in [TODO.md](TODO.md);
validation evidence lives in the [work ledger](docs/work-ledger.md).

## Unreleased

v0.1.3 remains untagged. Event-start grouping is being corrected to the requested
configurable boundary model; the existing heuristic behavior below is integrated
but does not satisfy that requirement.

### Features

- Collapse common tracebacks, wrapped log messages and pretty-printed payloads
  into grouped events with Grouping (`m` → Auto). Each row shows its first line
  and constituent count; Enter expands or collapses the lines. Custom accepts
  a continuation rule, and Off restores individual-record presentation.
  Optional Folding (`z`) can then collapse consecutive similar events.
  Auto uses conservative recognition, with at most 64 physical records /
  64 KiB per group and nondecreasing capture times within 30 seconds of its
  head. An oversized physical record stays standalone; unfamiliar structure
  may stay ungrouped. Original bytes, identities and filter membership remain
  unchanged.

### Fixes

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

- None recorded.

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
