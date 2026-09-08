# lvu detailed implementation plan

Current implementation status and remaining tasks: [TODO.md](../TODO.md).

Status: implementation is in progress. See [the architecture map](architecture.md)
for current components and [README](../README.md) for supported preview behavior. This document records the full requested scope, including work not
yet delivered. `work-ledger.md` records verified progress and `previews.md` records
the capabilities and limitations of each published build.

## 1. Outcome and scope

Build a fast local terminal log workspace with sources, independent saved views,
progressive structure, convenient time handling, and agent assistance. The main
flow is add/discover source → immediately read raw output → enrich/filter/pin/
color → reuse a remembered view → investigate the exact data using a local agent.

Sources are acquired once and shared by multiple views; views can merge sources.
The UI is a source/view selector beside a virtualized log pane, with optional
event details and editors. All primary actions have keyboard and mouse paths.
The application remains useful while the optional agent integration is offline.

Stack: Rust, Ratatui/Crossterm, Tokio, Rust Polars; a Python helper constructs and
serializes familiar Python Polars expressions; a TypeScript bridge controls local
Paseo sessions using existing coding-agent subscriptions. SQLite stores operational
state and TOML stores editable named definitions. Pin verified dependency versions
and lockfiles. Validate API assumptions against installed code/official sources.

Use mise for all tool pins, environment variables and developer/test tasks. Root
mise.toml is primary-owned. Start with minimal Rust plus rustfmt/clippy, Python,
Node and uv; component commands are exposed through mise as implementations land.

Explicit exclusions: a headless product mode; built-in plotting/notebooks/general
analytics; jq/yq as built-in languages; ACP or alternative agent backends; remote
agent data staging; per-provider authentication validation. Arbitrary command
enrichment enables external jq/yq/scripts without another core language engine.

## 2. Invariants and failure semantics

1. Preserve raw bytes, source identity, record order, delimiters, and invalid text.
   Derived display text never replaces authoritative capture.
2. Parsing/enrichment never removes records. Only explicit view filters affect
   visibility. Pending enrichment is visible as pending, not counted as no-match.
3. Show raw output without waiting for perfect schema, completed indexing, AI,
   enrichment, or a valid editor draft.
4. Invalid drafts retain last applied configuration. Query revision fences prevent
   cancelled older scans from replacing newer results.
5. Cache cleanup never deletes a sole durable copy. No silent data eviction.
6. Preserve record alignment through native enrichment; row count alone is
   insufficient to validate an expression that could reorder values.
7. Capture boundaries and dependencies make investigations reproducible while
   live acquisition continues.
8. Bound memory, channels, batches, processes, outstanding queries, output, and
   redraw cadence. Source-specific backpressure and failures are observable.
9. Do not display success for missing SDK support, skipped live integration, or
   absent terminal checks. Record practical limitations with evidence.

## 3. Repository and execution boundaries

*Superseded: the crates that exist are listed in `architecture.md`; this
proposal named a `lvu-state` crate that was never created and omitted six
that were. Kept for the boundaries it argued for, not the layout.*

Proposed workspace (foundation agent may simplify module granularity):

```
crates/lvu-core/        identities, source/view definitions, journal, capture
crates/lvu-query/       batch construction, native Expr validation/execution
crates/lvu-discovery/   Docker, process/open-file, project and recent discovery
crates/lvu-state/       SQLite, TOML, ownership and cache policies
crates/lvu/             application state, workers, Ratatui, subprocess lifecycle
python/                pinned expression compiler helper
bridge/                TypeScript Paseo adapter and tests
tests/                 fixtures, PTY workflows, integrated acceptance tests
scripts/               reproducible developer validation/benchmark commands
docs/                  plan, contracts, work ledger, technical decisions
spikes/polars-interop/  isolated compatibility proof, retired or reused after gate
```

The UI consumes bounded page results and progress events, never a Python object
per stored record. Acquire bytes → journal → tolerant batches → versioned stages
→ matching IDs → viewport formatting/styles. Keep ingestion tasks separate from
query workers and terminal event handling. Cache compiled predicates per revision.
Use immutable sealed segments with a small mutable/hot tail. Historical scanning
is cancellable between bounded batches; uncertain cancellation inside Polars is
isolated with limited concurrency and stale-result fencing.

## 4. Acquisition and journaling

Implement framed append-only records with length, IDs, payload and integrity
checking; recover committed records and detect/truncate an incomplete tail only
under explicit recovery rules. Document durability level (buffered write versus
fsync) and flush cadence. Sequence IDs cannot be reused after restart.

File sources support initial read, tail, partial lines, rotation by inode/handle,
and truncation. Drain the previous rotated handle where possible. Do not mistake
a same-path replacement for continuation. Capturing files is a snapshot/follow
operation, not a promise to reconstruct already overwritten input.

Commands capture stdout and stderr independently, preserve stream labels and exit
status, record start/restart boundaries, and own their child lifecycle. Ordering
across pipes is observed arrival order. Stop/cancel cleans up only owned processes.
Support explicit shell commands and executable/argument definitions. Do not
interpolate data into shell source. Keep cwd and environment overrides explicit.

HTTP sources specify framing (initially newline and/or SSE where tested), reconnect
policy and observable discontinuities. Command sources can cover integrations
before dedicated HTTP support is ready; do not mark HTTP complete on that basis.

Very long lines require chunked storage/framing rather than silent truncation;
rendering and enrichment can use bounded projections with diagnostics while raw
bytes remain. Invalid UTF-8, ANSI escapes and carriage returns are normalized only
for display. Expose acquisition lag, exit, reconnect, backpressure, disk limit.

## 5. Tolerant structure and query engine

Recognize JSON objects and logfmt conservatively, preserving other lines. Keep
original values separate from typed column projections. Mixed type batches must
not fail ingestion. Define nested key addressing, reserved names, null/missing
semantics and schema evolution with fixtures before UI autocomplete depends on it.

Python constructs native Expr JSON once per edit. Rust deserializes, validates,
caches and runs it. Pin a compatible Python/Rust pair based on actual tests, not
matching version numbers by assumption. Compiler requests time out and can restart;
diagnostics do not corrupt stdout protocol. Keep source expression text portable.

Initial allowed live expressions are row-local arithmetic, comparison/boolean,
string/regex extraction, tolerant casts, nested field access and timestamps.
Reject callbacks, plugins and operations that change order/cardinality or depend
on global state. Add aliases only to non-protected fields. Compare native Python
and Rust results including nulls and types. SQL is optional later convenience via
Polars; do not block primary Python expression support on adding it.

Enrichment recipes contain stages; expressions in a stage use its input, later
stages can depend on its output. Track independent failures; mark dependent fields
unavailable. Preserve raw rows after whole-batch engine failure. Preview diverse
patterns, rare forms, missing fields and failures; label sampled coverage honestly.
History recomputes by revision in background. Schema and revision changes never
silently mix interpretations in a committed view result.

Multiline grouping references every physical record, exposes provisional groups
immediately, and has explicit start/continuation/flush rules and raw-lines toggle.
Grouping changes are versioned separately from bytes and event selection mapping.

Command enrichment uses the ID-carrying JSONL contract. Test missing/duplicate/
unknown IDs, invalid JSON, protected fields, huge output, stderr pressure, hung and
exited processes. Retain its results as durable derived capture by default, since
arbitrary commands may have side effects or change results on replay.

## 6. User interface and navigation

Source/view sidebar: source health, named child views, merged views, add/discover,
rename, duplicate, stop/restart. Adding a view never starts another capture.
Viewport: stable selection, live toggle, paging, wrap/truncate, horizontal scroll,
configurable pins, details, match progress, pending/error counts. Retain each
view's scroll/anchor and use IDs to avoid selection jumping when data arrives.

Editors: source dialog with path/command completion, cwd/environment; filter and
enrichment editors with discovered keys/values, inline validation, last applied
definition, before/after rows, undo and Ask AI. Pasting multiline expressions must
work. Drafts persist separately. Palette exposes keyboard actions; help documents
bindings. Mouse supports selection, scroll, resize, detail expansion and field
actions (include/exclude/pin/color/correlate).

Time: capture versus event timestamp, original text and timezone assumptions;
absolute/relative range, dataset-relative last N minutes, ±context around event;
UTC/local/relative/elapsed displays; gap navigation; optional activity strip for
time selection (navigation, not an analytics/plotting feature). Live windows age
out old matches even when no new rows arrive. Merged arrival/event-time ordering
states handling of late events and skew.

Style: severity, stable per-value color, regex spans for visible text and native
predicate color rules. Distinguish missing/pending/error values. Field explorer
shows observed types, sampled/exact counts and common values. Context reveals
neighboring records without replacing the view filter. Correlation searches shared
fields across sources. Bookmarks/notes retain IDs. Pattern folds are reversible,
show count and first/last occurrence, and retain all constituent events.

Terminal handling restores modes on exit/errors, supports resize and small sizes,
Unicode widths and combining characters, bracketed paste, mouse and color fallback.
Never populate a widget with all retained rows. Initial compatibility target is
Linux locally and through tmux/SSH; macOS/Windows checks broaden with available CI.

## 7. Discovery

Searchable asynchronous provider list: Docker containers with labels/Compose
service/project/context, processes and writable regular files, project candidates,
recent sources. Discovery is read-only, cancellable, bounded in breadth/time and
tolerates permission errors and races. Results include evidence, confidence,
identity hints and a source definition ready for the ordinary dialog.

Docker uses structured CLI/API output, status and history/follow options. No
container mutation for real discovery; integration fixtures own their containers.
Process discovery first targets Linux /proc: arguments, cwd, parent relations,
tee filenames, regular writable descriptors and redirected stdout/stderr.
Never consume another process's pipe. Resolve relative tee paths using process
cwd and corroborate with open files. Platform providers degrade with clear status.
Deduplicate by canonical identity, preserving evidence from all providers.

Stable identity is separate from PID/inode/container instance: use logical path,
project and Compose service where available, with explicit disambiguation to avoid
applying unrelated remembered recipes. AI ranks/adapts candidates but deterministic
discovery does not require AI.

## 8. Persistence, caches and cleanup

TOML: versioned editable source definitions and named recipe/view definitions with
stable IDs, descriptions, Python expression text, presentation and identity hints.
SQLite: autosaved working views/drafts/navigation, revisions, capture catalog,
ownership pins, usage history, discovery fingerprints and suggestion outcomes.
Atomic TOML replace and transactional database migrations; define conflict rules
for external TOML edits instead of silently overriding them with database state.

Managed memory target initially 512 MiB; disposable disk target 5 GiB, both
configurable. These budgets exclude separately tracked durable captures and are
not process RSS promises. Account allocations by bytes, including match indexes;
spill or compact history indexes. Bound batch size and concurrent query working
sets. Evict unused LRU cache entries keyed by revisions and segment identity.

Storage screen shows raw/derived durable data, disposable cache, investigation
pins and reclaimable bytes. Cache clear touches only managed disposable files.
Source/investigation deletion releases ownership transactionally; pinned shared
segments survive. Interrupted cleanup and restart recover consistently. Protect
against path traversal/symlink escape in cleanup. Avoid synchronous mass deletion
on editor changes; obsolete cache entries become candidates for background work.

No automatic capture deletion by default. Explicit optional rolling retention
shows gaps/boundaries. At quota/disk exhaustion reclaim safe cache, then stop or
backpressure acquisition with a recorded error. Disk capacity cannot guarantee
unlimited continuous capture; UI must accurately expose the interruption.

Memory retrieval starts with project/source metadata, fields/types, patterns,
recipe usage and accept/reject history. Named recipes are portable; no embedding
service required initially. Exact-source state restores automatically; adapted
recipes remain proposals with evidence, compatibility and coverage preview.

## 9. Local AI and investigations

Paseo bridge owns session creation, prompt/event streaming, cancellation, resume,
capabilities, errors and typed proposals. Read installed SDK or official docs for
actual supported behavior. One available working provider establishes shared
integration. Do not ask user to authenticate each provider or build auth tooling.

Ask AI contexts include current definitions/revisions, schema and diverse sampled
Parquet plus local paths for complete source/view inspection. Response schema
selects source, filter, enrichment or view configuration. Validate structure and
compile/evaluate expressions locally; previews show effects. Reject stale revision
application or ask to rebase proposal against current view. Keep normal viewing
working through unavailable bridge, delayed responses and cancellation.

Investigation freezes source high-watermarks and time bounds, materializes filtered
enriched Parquet including raw text and IDs, exposes full-source context and exact
definitions/Polars semantics, and provides a working directory. Preserve raw byte
references or binary data for lossless context. Atomic manifests record complete
files and ownership pins. Export progress is cancellable and partial export cleanup
is safe. Agent chooses analysis/plotting tools; no required load.py/view.py programs.
Store session association, support resume and explicit refreshed snapshots. Data
survives source cleanup until investigation ownership is released.

## 10. Autonomous testing and review

Tests run without user-operated UI. Build deterministic fixture producers with
known raw bytes/IDs: JSON/logfmt, malformed JSON, mixed/nested values, multiline
traces, partial writes, invalid UTF-8/ANSI/CR, long lines, rotation/truncation,
bursts, command exits. Inject short writes/disk limits/restarts where feasible.

Layer A: unit/contract tests for meaningful state and storage invariants. Layer B:
cross-language differential expression fixtures comparing values, nulls and dtypes.
Layer C: real journal/acquisition/query/persistence integration tests. Layer D:
Ratatui TestBackend actions and snapshots with assertions on selection/styles.
Layer E: actual PTY binary workflows with deterministic keys/mouse/paste/resize,
terminal screen parsing and timeout diagnostics. Layer F: benchmarks and local
Paseo integration with an actual inspected dataset and typed returned proposal.

Mandatory PTY stories: add file; add live command; discover/open controlled tee
file; add second independent view; valid and invalid filter; enrichment preview
with failures; pin/color; switch/history/live; restart and restore applied state
and draft; AI proposal preview; export/resume investigation. Check outcomes using
captured screen plus persisted data, not sleep-only tests or ANSI substring luck.

Discovery tests use fake /proc fixtures plus a real owned tee pipeline; Docker
uses owned disposable containers if available, otherwise report skipped live
Docker gate. Paseo fake adapter covers protocol faults, but cannot satisfy live
gate. Test one real provider; no provider matrix requirement.

Provisional performance gates on documented hardware: p95 UI action under 100 ms
and raw display around 100 ms on target workload, no unbounded managed cache growth,
useful first screen before complete multi-GB indexing, cancellation acknowledgement
within a bounded batch, no raw byte loss below source/storage limits. Record bytes/s,
event shape/size, batch duration, CPU/RSS, cache bytes, cold versus warm scans and
repaint latency. Establish actual baselines before asserting numeric performance.

Primary review: read complete diff and interfaces; rerun targeted checks; inspect
failure semantics, cleanup, IDs, bounds and process lifecycle; exercise integrated
PTY story; require corrections in owner worktree; integrate coherent commit only
after gate. Run full fmt/test/clippy/typecheck suites when integration affects shared
paths. Keep concrete commands/results and skipped checks in ledger/reports.

## 11. Milestones and work packages

M1 Architecture proof (dependency root):
- M1A core model, lossless journal/recovery, file/command acquisition tests.
- M1B Python/Rust Polars exact-version proof and native expression restrictions.
- M1C TypeScript local Paseo protocol, deterministic tests and one real round trip.
- M1D executable Ratatui shell, viewport/state split and PTY harness.
- Exit: live fixture shown, original bytes verified, Python-authored expression
  executed in Rust, AI returns typed expression using actual local sample, PTY runs.

M2 Daily-use viewer (requires integrated M1 contracts):
- independent views/merged source references, JSON/logfmt and schema projections;
  real filter editor, autocomplete/errors/last-valid; details/pins/colors/time;
  follow/history/rotation and minimal autosave.
- Exit: multi-service scripted incident isolation, independent filters, restart
  restoration; bounded rendering while capture and historical scans run.

M3 Discovery (can overlap M2 after SourceDefinition settles):
- Docker provider and controlled fixture; process/tee/open-file provider;
  project/recent provider; dedupe/ranking and searchable source dialog.
- Exit: autonomous live tee discovery and open, Docker fixture where available,
  races/inaccessible process handling, no pipe consumption.

M4 Robust enrichment (requires M1B + M2 structure):
- staged expression UI/preview/history, tolerant mixed fields and dependencies;
  multiline framing; managed command enrichment and failure diagnostics.
- Exit: malformed corpus remains intact, revisions recompute with no stale commit,
  every command protocol fault preserves original and bounds outstanding work.

M5 Durable policy and memory (foundation begins M1/M2):
- cache budgets/eviction, capture catalog/pins/storage screen/cleanup; SQLite
  migrations/revisions/drafts, named TOML recipes/source identity and retrieval.
- Exit: stress under tiny budgets, crash/restart recovery, deletion with shared
  investigation ownership, external TOML round trip and precise restored drafts.

M6 AI assistance (M1C + M4 + M5):
- typed action contexts/Parquet samples, proposal validation/preview/apply;
  iterative failed-example assistance, similar-source adaptations and memory.
- Exit: actual local agent derives useful filter/enrichment from fixture data;
  stale/malformed/cancelled/unavailable cases handled; saved result works without AI.

M7 Investigations and navigation completion (M2/M4/M5/M6):
- fixed Parquet export/full context/manifest/pins/session resume/refresh;
  correlation/context/bookmarks/notes/folding; terminal and performance hardening.
- Exit: exact filtered dataset matches Rust result; export and agent use/resume;
  source cleanup preserves investigation; end-to-end incident PTY regression passes.

M2 is the first daily-use release, M4 the core viewer, M7 the agreed complete scope.
Do not describe implementation as complete while any exit gate remains unverified.

## 12. Delegation, integration and git policy

*Historical: the three initial worktrees are long merged. The rules that
survived are in `AGENTS.md`, Coordination.*

Primary agent owns docs/contracts/work ledger, assignment boundaries, root test
orchestration and final integration. Start three independent isolated worktrees:
core foundation, Polars proof, and Paseo bridge. Shared Cargo workspace/lockfile is
owned by core initially; the Polars proof uses its own isolated manifest. Bridge
owns bridge/package manifests only. Root does toolchain setup and review/test plans.

After initial results, reconcile interfaces before assigning query/TUI/state work.
Use a fresh branch from reviewed main for each new chunk or explicitly update old
worktree base. Do not let competing agents edit the same module/manifest. An agent
may propose a cross-boundary change but primary assigns it before editing.

Each work package returns commit IDs, changed files, tests/commands/results,
environment assumptions and unresolved concerns. Review corrections amend or fixup
unpublished topic commits. Integration may squash fixups into the relevant logical
change. Preserve readable commits with concrete behavior and validation in body;
do not spam main with experiments/build-error corrections. No forced rewrite of
published or user-owned history. No push/deploy implied by local git commits.

Paseo handoffs use the configured GPT-5.6-Sol-Implementer profile with complete
briefings and notify-on-finish. Agents remain attached subagents. Continue useful
local work; completion notifications trigger review and next chunks. No status
polling loop. The user explicitly authorizes continued implementation supervision.
