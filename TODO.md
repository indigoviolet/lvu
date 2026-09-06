# lvu remaining work

Last updated: 2026-09-06. Published preview: **038-json-fields**.

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

- [ ] **Consistent dialog forms, starting with Time and enrichment.** Group
  related inputs horizontally when space permits; distinguish bounded inputs,
  visible action buttons, colored applied/pending/error status and secondary
  help. Use finite-choice dropdowns and only show scrolling controls for real
  overflow. Time needs Start/End rows and timezone selection; enrichment still
  needs Add/Edit/Remove controls and ordinary-language labels. Existing editor
  work and the wider dialog audit are coordinated, not published in preview038.

- [ ] **Consistent layered dismissal.** q and Escape close the innermost active
  layer before quitting from the workspace. q remains text in editable fields.
  Preserve pending-operation ownership and clipboard behavior; implementation
  and per-layer terminal acceptance are assigned.

- [x] **Remove redundant Help entries.** Drop basic row-navigation explanation,
  the non-command Review row and Mouse section. Integrated on main; supervisor
  UI and Help PTY checks pass. Main composed acceptance passes; published in preview038.

- [x] **Bottom-left in-pane heartbeat.** Full-height sidebar border, reserved
  list/hitbox space and bottom-left 5x3 heart. UI and actual terminal checks pass;
  included in preview037 alongside Time/navigation.

- [ ] **Keep assistance sessions out of the user's workspace list.** Group helper
  sessions under one lvu workspace and archive settled ephemeral helpers while
  retaining inspectable activity. Preserve explicitly resumable investigations.
  SDK lifecycle/grouping capabilities are under review; no cleanup is published.

- [ ] **Compact full snapshot export.** Stop duplicating physical schemas per
  part; pack compatible processing batches into larger Parquet files/row groups.
  Keep replay boundaries, source order, schema changes and disk/memory limits
  explicit. Full investigation/export work is separate from short assistance.
  Implementation assigned; no accepted integration yet.

- [x] **Keyboard navigation without extended keys.** Removed PgUp/PgDn/Home/End
  bindings and hints, retaining focused arrow/scroll navigation. Search's empty
  universal-key footer is removed. Tests and actual Help/Search/command Details
  workflows pass; published in preview037.

- [x] **Time dialog form.** Prefilled date/time/timezone segments, staged
  basis/window dropdowns, focusable actions and separate applied status. Drafts
  and custom intervals persist; UTC offsets and nanosecond precision survive
  editing. Actual narrow/offset/arrivals/restart/Clear workflow passes. Published
  in preview037; no universal-key reminder strip.

- [x] **Automatic JSON coloring.** Consistent key colors and value styles, with
  preserved text, selection contrast, Unicode clipping and clipboard contents.
  Reviewed source integrated on main; component tests and fixture PTY pass.
  Actual file-capture PTY and main composed acceptance pass. Truecolor
  is the verified color path; unknown terminal backgrounds have no measured
  contrast guarantee. Published in preview038.

- [x] **Event fields feedback.** Opening Fields with no parsed fields or no
  selected record shows an explanatory dialog. Pending data retains a stable
  record identity; raw context remains accessible. Main source integrated without
  correlation dependencies; composed plain-source PTY passes. Published in preview038.

- [x] **User-provided title and corner heartbeat.** Large sharpened Chafa title
  embedded with 110 ms frame timing, true-black canvas, responsive fallback,
  any-key consumed dismissal and CLI bypass. Four-frame corner sprite animates
  real work; routine idle/working labels removed. UI, real-source and actual
  color terminal checks pass. Published in preview033.
- [x] **Consistent help/dialog hierarchy.** Grouped themed Help adapts columns and
  owns its scrolling. Search separates editable input, examples, state and the
  last accepted filter. Source assistance and Time/Recipes/Ask actions remain
  discoverable; Discovery/Storage diagnostics scroll separately from action
  footers. Tiny layouts preserve inputs and explain when enlargement is needed.
  UI tests and actual color-enabled/help/Search plus full app PTYs pass. Published in preview033. See [dialog presentation](docs/dialog-design.md).

- [x] **Product README and separate development reference.** README leads with
  user benefits, quick start, supported features, platforms and current limits.
  Build/test internals live in `docs/development.md`; architecture, contracts and
  the ledger retain implementation details. In-app Help and the command palette
  are the control reference. AGENTS.md records extension and validation guidelines.
- [x] **Broaden expression support without duplicated method filters.** Source
  uses Polars IR elementwise metadata before publication and against real schemas,
  preserving alignment/batch independence and runtime bounds. Python constructors
  exclude eager/I/O/callback tooling. Datetime constructors, strings, list element
  extraction and temporal/coalesce expressions pass native value/partition checks;
  invalid edits preserve the live chain, including empty-capture rejection.
  Native feature limits remain explicit. Published in preview033.
- [x] **Confine drag selection to the active dialog/pane.** Source integration
  passes actual cross-boundary drags in both directions with exact OSC 52 contents,
  plus narrow/tiny terminal geometry checks. Published in preview033.
- [x] **Repair redraw after resize/reflow and live arrivals.** Source integration
  invalidates every resize and adds Ctrl-L recovery, synchronized draws and wrap
  restoration. Actual PTY resize round-trip/live-arrival checks pass. The injected
  stale-cell test is not a reproduction of the user's exact screenshot. Published in preview033; retain any further corruption reports for diagnosis.

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
- [x] **Explicit timestamp sampling.** Source manifests specify evenly spaced
  first-to-last row offsets, at most 128/source and 512 total, preferring applied
  typed outputs with explicit source-context fallback. Outgoing proposal schemas
  bind exact revisions. Actual Luna reported 128-of-500 coverage and produced a
  directly sourced UTC expression; native output/nulls and saved restart passed.
  This requests coverage, not an enforced provider I/O cap. Published in preview033.
- [ ] **Fast, bounded timestamp/definition assistance.** Query the frozen typed
  data before full export and put schema, representative values, revisions and
  coverage directly in the prompt. Cap serialized bytes as well as rows; expose
  omissions and a prepared bounded query/helper for larger inspection. Avoid full
  Parquet export for short Ask requests and agent-driven sample assembly. Add
  readable session activity. Preparation and app/protocol integration are assigned;
  no optimization or new live-provider acceptance has completed. Previous diagnosis
  found seven tool calls across 40 listed parts for a requested 128-row sample.


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

- [ ] **Portable installation through Homebrew and mise.** Main is on GitHub.
  Relocatable helper discovery, runtime provisioning, target release archives,
  clean-install/platform acceptance and a Homebrew tap remain. See the
  [distribution exploration](docs/distribution.md); no package release exists.

- [x] **Reviewed terminal command enrichment.** One optional command
  follows native stages, with frozen input review, explicit confirmation, durable
  attempt reservations, typed read-only Details results, pending new arrivals and
  restart without execution. Failed runs retain the last published result set.
  Combined Rust, SQLite/subprocess and copied-app PTY acceptance pass; published
  in preview034. Schema v4 is incompatible with preview033/older.
  See [command enrichment](docs/command-enrichment.md) for limits and controls.
- [ ] **Command enrichment extensions.** Multiple/interleaved command stages,
  command-output-dependent native queries and recipe persistence remain unsupported.
  Preserve explicit execution and durable attempt ownership when extending them.
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
- [ ] **Field correlation across sources.** Native comparison/provenance and
  Fields UI source are reviewed on the supervisor branch. Record lookup, query
  routing, persistence/controller integration and end-to-end acceptance remain;
  this is not yet an integrated main feature. Some wiring is parked while
  assistance improvements take priority.
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
