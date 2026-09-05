# lvu remaining work

Last updated: 2026-09-05. Published preview: **029-context-controls**.

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
- [x] Publish preview029 with raw context, explicit capture controls and verified
  Luna timestamp assistance after copied-binary PTYs. Previous previews remain.

## Next: correctness and daily use

- [x] **Use an extracted timestamp in the Time dialog.** Alt-U explicitly selects
  the accepted `timestamp_utc` enrichment. Filtering, live arrivals, selected-event
  anchoring, persistence, recipe round trips and exact snapshot timestamps pass
  tests. Published in preview028.
  Arbitrary timestamp-field names remain a later extension.
- [ ] **Investigate the intermittent empty plain-file reopen.** One combined-load
  gzip PTY run showed no rows for a plain file; a focused rerun and 80 subsequent
  opens/reopens passed. Four further concurrent suites (32 opens/reopens) passed.
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
- [ ] **Merged multi-source views.** Add creation/editing and persisted source
  membership, with explicit ordering and stable selection. Existing independent
  named views already share their source's capture.
- [x] **Neighboring-record context.** `o` opens bounded raw source context around
  a fixed selected record; scrolling and live arrivals leave the filter intact.
  Native journal, small-terminal and real PTY tests pass.
- [ ] **Navigation tools.** Field correlation across sources and persisted
  bookmarks/notes remain.
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
- [ ] **Recipe workflow completion.** Review immutable revision/history and update
  UX against the plan. Expand suggestion adaptation beyond the
  advanced filter only after whole-chain transactional validation is supported.
- [ ] **Ownership-aware retention.** Complete source/investigation deletion and
  shared ownership pins, optional capture retention with visible gaps, and safe
  cache eviction under pressure. Current cleanup is explicit and limited to
  verified unused derived indexes; it never deletes captures or investigations.
- [ ] **Performance and terminal acceptance.** Measure sustained capture plus
  historical scans and rolling-window refreshes under small budgets; exercise
  tmux/SSH. Add persistent query checkpoints only if measurements justify them.
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
