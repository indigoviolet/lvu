# lvu remaining work

Last updated: 2026-09-05. Published preview: **026-search-extraction**.

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
- [ ] Publish the next immutable preview after these changes pass validation.

## Next: correctness and daily use

- [ ] **Use an extracted timestamp in the Time dialog.** The current Alt-T action
  prepares a reviewed `timestamp_utc` enrichment prompt, but event-time filtering
  still recognizes original raw fields. Add explicit timestamp-field selection;
  carry the choice through filters, live arrivals, recipes, restart and snapshots.
  Malformed/missing values must stay explicit, without guessed timezones.
- [ ] **Investigate the intermittent empty plain-file reopen.** One combined-load
  gzip PTY run showed no rows for a plain file; a focused rerun and 80 subsequent
  opens/reopens passed. Root cause remains unknown. Reproduce under load using
  retained failure artifacts and distinguish capture, index and publication state.
- [ ] **Complete field-search addressing.** Current shorthand uses `field: value`
  with a restricted field-name alphabet. Support fields containing spaces or
  punctuation unambiguously, and document escaping literal leading `/` text.
  Preserve debounce, clear-to-empty and last-valid behavior.
- [ ] **Verify timestamp assistance with an actual model proposal.** Native UTC
  conversion and prompt preparation pass tests; the new timestamp-specific
  model request/application has not had a live-provider acceptance run.

## Remaining planned product work

- [ ] **Command enrichment integration.** The bounded subprocess/protocol crate is
  built; connect it to ordered stages, explicit execution, persistence and durable
  attempt tracking. Retrying or reopening must never silently rerun attempted IDs.
- [ ] **Merged multi-source views.** Add creation/editing and persisted source
  membership, with explicit ordering and stable selection. Existing independent
  named views already share their source's capture.
- [ ] **Navigation tools.** Add neighboring-record context without replacing the
  filter, field correlation across sources, and persisted bookmarks/notes.
- [ ] **Time-navigation extensions.** Dataset-relative ranges, gap navigation and
  explicit display modes; define late-event/skew behavior before event-time sorting.
- [ ] **Presentation extensions.** Predicate color rules, regex span highlighting,
  richer field/type/value exploration, and repeated-pattern folds. Stack-trace
  grouping, severity colors and stable color-by-value already work.
- [ ] **Source lifecycle UI and acquisition gaps.** Complete explicit stop/restart
  controls where absent; HTTP/reconnect and non-`Never` command restart remain
  unsupported and must not be accepted silently.
- [ ] **Recipe workflow completion.** Review immutable revision/history and portable
  export/update UX against the plan. Expand suggestion adaptation beyond the
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
