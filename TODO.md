# lvu remaining work

Last updated: 2026-09-06. Published preview: **032-inspect-and-copy**.

This is the current execution checklist. The [implementation plan](docs/implementation-plan.md)
defines the wider scope; the [work ledger](docs/work-ledger.md) records evidence and
[preview notes](docs/previews.md) identify what is actually in each binary.
Unchecked items are not claims of working features. Update this list after each
implementation checkpoint and preview publication, including new bugs and changed scope.

## Current checkpoint

- [x] Default new 🧠 configurations to `codex/gpt-5.6-luna`; preserve explicit saved
  model choices and environment overrides. Default, round-trip and override tests pass.
- [x] Remove the footer's adjoining pulse trace. Animate only the heart with a
  filled/outline double beat, fixed label position, and static reduced-motion mode.
  Rendering and real terminal tests pass, including reduced motion and restoration.
- [x] Publish preview031 with immutable recipe history/updates and ordered
  adaptation after copied-binary and SSH PTYs. Previous previews remain.

## Next: correctness and daily use

- [x] **Current architecture and feature documentation.** README now lists supported
  preview032 behavior; `docs/architecture.md` maps component ownership, data flow,
  revisions, persistence, execution boundaries and limits. AGENTS.md records
  extension and validation guidelines.
- [ ] **Broaden expression support without duplicated method filters.** Evaluate
  Polars plan semantics while preserving record alignment and batch independence;
  keep runtime bounds/shape checks as a backstop. `pl.Datetime(...)` support is
  prepared and targeted tests pass, but is not yet published.
- [ ] **Confine drag selection to the active dialog/pane.** Working-tree geometry
  and selection changes need integrated terminal acceptance and publication.
- [ ] **Repair redraw after resize/reflow and live arrivals.** Working-tree terminal
  invalidation/recovery changes need actual PTY acceptance; the reported screenshot
  is not yet attributed to a conclusively reproduced root cause.

- [x] **Visible text selection and copy.** Drag selects the composited screen;
  Ctrl-C sends the selected text through OSC 52. Escape dismisses the dialog and
  clears selection. Clipboard delivery depends on terminal support. Copy PTY passes.
- [x] **Readable enrichment editing.** Larger terminals show saved steps, a
  multiline draft, validation status and separate input/output panes. String
  `replace`/`replace_all` pass Python-to-Rust execution tests.
- [x] **Prefer structured timestamp inputs.** Proposal instructions now require
  inspecting actual typed fields/values before extracting from raw; no input field
  name is assumed. Schema/bridge tests and a live Luna proof using `observed_at`
  pass: the returned expression reads that column directly and normalizes UTC.
  Published in preview032.
- [ ] **Explicit timestamp sampling.** Current proposals choose their own bounded
  samples from the fixed snapshot; there is no enforced sample count. Specify
  diverse per-source sampling and report proposal validation coverage.


- [x] **Use an extracted timestamp in the Time dialog.** Alt-U explicitly selects
  the accepted `timestamp_utc` enrichment. Filtering, live arrivals, selected-event
  anchoring, persistence, recipe round trips and exact snapshot timestamps pass
  tests. Published in preview028.
  Arbitrary timestamp-field names remain a later extension.
- [ ] **Investigate the intermittent empty plain-file reopen.** One combined-load
  gzip PTY run showed no rows for a plain file; a focused rerun and 80 subsequent
  opens/reopens passed. Four further concurrent suites (32 opens/reopens) passed.
  Another 128 opens/reopens passed with four test processes pinned to one CPU.
  Root cause remains unknown. Reproduce under load using
  retained failure artifacts and distinguish capture, index and publication state.
- [x] **Complete field-search addressing.** JSON-quoted field names support spaces,
  punctuation, escaped quotes, Unicode and empty names within the existing 64-byte
  field-name limit. `\/` selects literal leading-slash text. Query tests and the real search-box regression pass.
- [x] **Verify timestamp assistance with an actual Luna proposal.** The live
  proposal applied through native validation and normalized a `+0200` timestamp to
  UTC. Explicit schema/expression instructions and the pinned Parquet reader are
  now supplied; malformed proposals still preserve the working view.


## Remaining planned product work

- [ ] **Command enrichment integration.** The bounded subprocess/protocol crate is
  built; connect it to ordered stages, explicit execution, persistence and durable
  attempt tracking. Retrying or reopening must never silently rerun attempted IDs.
- [x] **Merged multi-source views.** `v`, Alt-M edits ordered open-source
  membership; clones preserve it. Changes publish atomically with accepted
  constraints and retain stable identities. Restart waits for explicitly opened
  sources without starting remembered commands. Rust and real PTY checks pass;
  published in preview032. Ordering is source position, then sequence.
- [x] **Neighboring-record context.** `o` opens bounded raw source context around
  a fixed selected record; scrolling and live arrivals leave the filter intact.
  Native journal, small-terminal and real PTY tests pass.
- [x] **Bookmarks and notes.** `b` toggles a stable record bookmark; `B` opens
  per-view bookmarks, notes and raw context. Restart, filtered-out records, note
  editing, removal, bounds and restore fencing pass Rust and real PTY checks.
- [ ] **Navigation tools.** Field correlation across sources remains.
- [ ] **Time-navigation extensions.** Dataset-relative ranges, gap navigation and
  explicit display modes; define late-event/skew behavior before event-time sorting.
- [ ] **Presentation extensions.** Predicate color rules, regex span highlighting,
  richer field/type/value exploration, and repeated-pattern folds. Stack-trace
  grouping, severity colors and stable color-by-value already work.
- [x] **Explicit source stop/restart.** Alt-S stops, Alt-R restarts the selected
  source while retaining its views and accepted filters. Files resume without
  duplicate records; commands restart only on explicit request. Stdin restart is
  refused without stopping the active pipeline. Rust and real PTY checks pass.
- [ ] **Acquisition gaps.** HTTP/reconnect and non-`Never` command restart remain
  unsupported and must not be accepted silently.
- [x] **Portable recipe export.** Alt-E in Recipes exports the exact selected
  immutable revision to a new TOML file. Existing files are preserved; importing
  into another workspace and applying the recipe passes real PTY checks.
- [x] **Recipe revisions and updates.** Alt-H reviews up to 100 immutable revisions;
  Alt-U explicitly saves accepted view settings as a new revision. Stale updates
  are rejected. Older revisions can be applied/exported without moving the current
  pointer. Rust and real PTY checks pass; published in preview031.
- [x] **Broader recipe adaptation.** Optional ordered enrichment definitions and
  the advanced filter apply atomically. Full proposal review, native dependencies,
  live arrivals, restart and an actual Luna proposal pass. Other recipe settings
  remain unchanged. Published in preview031.
- [ ] **Ownership-aware retention.** Complete source/investigation deletion and
  shared ownership pins, optional capture retention with visible gaps, and safe
  cache eviction under pressure. Current cleanup is explicit and limited to
  verified unused derived indexes; it never deletes captures or investigations.
- [ ] **Performance and terminal acceptance.** A reproducible 61,000-record
  small-budget baseline, isolated tmux and authenticated localhost SSH
  context/bookmark PTYs pass; see [measurements](docs/performance.md).
  A two-minute 481,000-record capture/query run also passes with bounded cache
  counters and final catch-up. Longer production workloads and latency-impaired
  remote SSH remain.
  Add persistent query checkpoints only if measurements justify them.
  In-session incremental append processing is already implemented.

## Explicit limits / later scope

- Gzip archives are static; changed archives need a fresh capture identity.
  Gzip stdin and live-growing compressed archives are not supported.
- Nested JSON expansion and general timestamp/epoch inference are not built.
- Memory budgets bound managed payloads, not total process RSS. The global disk
  cache cap applies to derived indexes, not durable journals or exported datasets.
- Polars cancellation is between bounded batches; active kernel I/O and native
  evaluation are not preemptible.
- Linux is the validated platform. Broader macOS/Windows support needs separate
  lifecycle, terminal and cleanup validation.
- Built-in charts, a headless product, jq, remote data staging and provider login
  tooling remain outside the agreed scope.

## Built and published through preview026

- [x] Durable file/command/stdin capture, exact bytes, resume and journal paging.
- [x] Gzip detection/decompression, unchanged reopen and corruption diagnostics.
- [x] Source discovery, positional CLI files, command flags, stdin and path completion.
- [x] Independent named views, working-state restoration and reusable recipes.
- [x] Literal/field/regex/expression search, 300 ms debounce and rapid-clear fencing.
- [x] Iterative ordered enrichments, named regex captures, dependencies and rollback.
- [x] Details, pins, colors, multiline grouping and horizontal log scrolling.
- [x] Capture/event-time windows, rolling presets and Time-dialog timestamp prompt.
- [x] Fixed Parquet snapshots, 🧠 proposals and resumable investigations.
- [x] Command palette and editor autocomplete.
- [x] XDG TOML settings, six themes, consistent dialogs and visible editable cursors.
- [x] Memory/index budgets, storage browser and reviewed disposable-index cleanup.
- [x] Persistent pixel-art startup title, skipped for CLI sources.

Completion means integrated behavior with appropriate tests, not merely a returned
component commit. Preview publication is tracked separately from source completion.
