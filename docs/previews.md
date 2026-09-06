# Development preview history

Previews are published after useful independently tested checkpoints. User feedback
is optional and never gates ongoing implementation. Each publication has a stable
versioned local binary and a manifest with its source commit, checksum, checks and
known limitations. Local retention is separate from historical acceptance.
`previews/latest` points to the most recent tested version; `mise run preview` runs it.
Binaries are local build artifacts and are not committed to Git.

Currently retained: **035–042**, with `latest → 042`. On 2026-09-06,
explicitly authorized cleanup removed bundles 001–034. Their entries below retain
source revisions, checksums and acceptance history; paths for those versions are
historical and no longer present locally. Captures, proofs and test logs were
untouched. Cleanup inventory: `/tmp/lvu-authorized-preview-cleanup-001-034.json`.

## 001: TUI demo

Historical executable (local bundle removed):
Historical executable: `previews/001-tui-demo/lvu --demo`.
Source commit: `852a855825d67c708009ced6415dcd7bf990e400`.

Synthetic logs only. Supports switching views, navigation, mouse selection/scroll,
details, follow/history, resizing and an editable filter draft. The query adapter
is unwired; filters do not execute. Real file/command sources are not connected.

Keys: `?` help, `q` quit, `[`/`]` views, `d` details, `f` follow,
`/` filter draft, `a` synthetic arrival. Arrow keys and mouse scrolling also work.

Primary independently reran ten Rust state/UI tests and the real PTY suite,
including viewport contents, input, resize, and normal/panic terminal restoration.
This preview does not imply acceptance of the complete production integration.

## 002: Live literal search

Historical executable (local bundle removed):
Historical executable: `previews/002-text-search/lvu --demo`.
Implementation: `e07bd8d` (owner `5a3974c`).

Press `/` and type `request 05`, `café`, or `late fixture` to narrow demo logs.
Search updates after 120 ms of idle typing. Punctuation is literal; case matching
uses Unicode lowercase. Press Escape to return to navigation. Reopen `/` and
backspace to empty to restore all rows. `a` supplies synthetic arrivals that obey
the active search. Views retain independent search state.

`p` opens the separate advanced Polars editor; execution remains unwired in this
preview. Real sources are still being integrated. Preview 001 is recorded above.

Primary reran 13 TUI tests and the actual PTY workflow after integration, including
search/no-match/clear/arrival behavior and normal/panic terminal restoration.

## Upcoming checkpoints

- Time filtering and cross-source recipe suggestions.
- Time navigation, multiline grouping, and richer enrichment editing.
- Reusable recipe controls and storage management.

Each checkpoint will document its actual supported behavior and a run command.

## 003: Real local sources

This version opens the Add source dialog. Tab switches file/command mode;
Enter starts capture. Historical executable: `previews/003-real-sources/lvu`.

```sh
./previews/003-real-sources/lvu --file /path/to/server.log
./previews/003-real-sources/lvu --command 'docker logs --follow my-container'
```

Repeat `--file` and `--command` to open several sources. Commands execute as
`sh -c` in the current working directory. Use `--capture-dir PATH` to choose
storage; the default is `.lvu-captures` in the current directory. Captured bytes
remain there after quitting. Reopening a file starts reading it again; historical
capture deduplication across acquisition runs is not implemented yet.

Keys: `n` adds a source, `[`/`]` switches views, `d` shows record details,
`f` follows arrivals, `?` shows help, and `q` quits outside editors. Escape closes
the source dialog/editor. File/command acquisition continues while browsing.

Native search and advanced filters report an explicit unsupported message in
this preview. Discovery browsing, saved view restoration, enrichment editors and
AI workflows remain pending. Multiline fragments appear as physical records;
merged time ordering and formatted timestamps are also still pending.

Primary validation: 18 UI tests, two app tests, demo PTY and real-source PTY,
startup-failure process cleanup, formatting, and clippy. Previous previews remain
unchanged. Local manifest records the exact source commit and binary checksum.

## 004: Discover local sources

Historical binary (local bundle removed):
`previews/004-source-discovery/lvu`.

From Add source, press Ctrl-D to browse discovery candidates. Type to narrow the
list, use arrows to select, and Enter to start the selected source. `n` opens Add
source while browsing logs. Candidates include Docker containers, writable log
files found through Linux process inspection (including tee), and project logs.
Unavailable providers show their errors; discovery never launches a candidate
without selection. Ctrl-D returns to manual entry.

Real file/command capture behaves as in preview003. Native log search is still
being connected; filtering the discovery list is available. Recent-source memory
is pending. Docker fixtures pass; this machine's live Docker socket is unavailable.

Primary validation: 19 UI tests, three app tests, demo PTY and real-source PTY
including actual tee/file discovery and explicit launch, formatting and clippy.

## 005: Search real logs and resume files

Historical binary (local bundle removed): `previews/005-real-search/lvu`.

```sh
./previews/005-real-search/lvu --file /path/to/server.log
./previews/005-real-search/lvu --command 'docker logs --follow my-container'
```

Press `/` and type to narrow actual captured logs. Search is literal,
case-insensitive, and updates for new arrivals. Each view keeps independent
constraints. Clear the search to restore all rows. Press `p` for an optional
Polars expression such as `pl.col("level") == "error"`; it combines with search
using AND. Invalid advanced definitions preserve the last accepted live view.
Advanced compilation uses this checkout's locked Python project through mise/uv;
literal search does not start Python. This is a local development binary, not a
standalone installation bundle.

Capture timestamps are displayed as UTC time. File cursors preserve acknowledged
positions across shutdown/reopen: unchanged files add no duplicate records and
appended files capture their suffix. Rotation, truncation and detected rewrites
produce explicit boundaries. Old captures created before cursor support have no
saved file position; use a fresh capture directory when testing resume behavior.

Discovery and source controls remain as in preview004. File-path completion,
automatic view restoration, enrichment editing, and agent workflows are still
being integrated. Match snapshots have a bounded payload budget; reaching it
reports a limit and preserves the previous view, without deleting raw capture.

Primary acceptance: 19 UI tests, four app tests, four native-view end-to-end tests,
18 core tests, 24 runtime tests and 14 paging tests; real-source search/advanced/
arrival/clear PTY, demo PTY, formatting and targeted clippy.

## 006: File-path completion

Historical binary (local bundle removed): `previews/006-path-completion/lvu`.

In Add source (`n`), type part of a file path and press Tab. Unique matches expand;
ambiguous matches appear in a list. Select with arrows and apply with Tab, then
press Enter to capture the file. Repeated Tab continues into a completed directory.
Spaces, Unicode, relative paths, `~/`, and directory symlinks are supported.
Alt-F selects file mode, Alt-C selects command mode, and Ctrl-D opens discovery.
Tab does not execute commands or perform shell completion.

Enumeration runs outside the UI thread, with limits of 1,024 inspected entries
and 64 candidates. Missing/inaccessible directories report inline errors. Delayed
results cannot replace newer drafts or a reopened dialog's input.

Includes preview005's real search and advanced filters, plus faster clean file
resume and terminal-status publication after the runtime lease is released.
View-memory restoration and recent-source history are the next active assignment.

Validation: UI/app tests, native view and capture/runtime tests, real-source PTY
including completed-path capture, demo PTY, formatting and targeted clippy.

## 007: Remember working views

Historical binary (local bundle removed): `previews/007-working-memory/lvu`.

Open a source, set a literal search or advanced Polars filter, and quit. Reopening
that source using the same capture directory restores its accepted constraints,
independent unfinished drafts, follow mode and saved selection where available.
Accepted filters execute again through the native dispatcher before being applied.
A delayed restore cannot overwrite newer user edits or navigation.

Memory lives in `<capture-dir>/workspace` (the default capture directory remains
`.lvu-captures`). SQLite writes run outside the UI, coalesce edits, and acknowledge
successful saves. Shutdown captures the final drafts even with a query pending,
then attempts a bounded 500 ms flush. Errors are surfaced rather than silently
reporting success; raw capture/browsing remains available when memory fails.

Add source -> Ctrl-D includes remembered sources. Selecting one explicitly opens
it; starting without source arguments does not relaunch remembered commands.
Remembered availability is unknown until checked by opening the source.

Named recipe editing, pins/colors controls, enrichment editing and agent workflows
remain pending. Previous previews remain unchanged.

Primary validation: 21 UI tests plus fixture, 12 app tests, 19 memory tests,
real restart/recent-source/in-flight-draft PTY, demo PTY, formatting and clippy.

## 008: Fields, pins, and colors

Historical binary (local bundle removed): `previews/008-field-presentation/lvu`.

Select an event and press `i` for its field picker. Use arrows or mouse to select
fields, Space/Enter to pin or unpin columns, and `c` to toggle stable color-by-value.
Escape closes the picker; `d` opens event details. The picker stays on the event
you opened even while live arrivals continue. Missing pinned values show `—`;
JSON null shows `null`. Severity is recognized from level/severity/lvl fields.

Pins and color selection persist per view alongside filters and drafts. Existing
v1 workspace memory migrates to v2 while preserving stored views. Older previews
cannot read v2 memory; use a separate capture directory when testing an older
binary after upgrading.

Recognition currently covers top-level scalar JSON and basic quoted logfmt.
Nested JSON expansion, richer escape handling, enrichment editing, and multiline
event grouping remain pending. Projection limits are 32 fields, 64-byte keys,
512-byte values, and eight pinned columns. Original captured bytes remain intact.

Primary validation: populated-v1 migration and reopen, field picker scrolling and
mouse geometry with live arrivals, UI/app/live/memory tests, real-source
pin/color/restart PTY, demo PTY, formatting and targeted clippy.

## 009: Native Polars enrichment

Historical binary (local bundle removed): `previews/009-polars-enrichment/lvu`.

Press `e` and enter a named expression, then Enter to apply:

```python
status = pl.col("raw").str.extract(r"status=(\d+)", 1).cast(pl.Int64, strict=False)
```

The editor shows representative raw lines and accepted derived values. Use `i`
to pin/color the new field and `p` to filter on it, for example
`pl.col("status") >= 500`. Unmatched extraction produces null without removing
original records. The accepted enrichment and unfinished draft survive restart.
Submit an empty enrichment definition to clear it; clear dependent filters first
if they require the field being removed.

Python compiles expressions on edits; Rust Polars executes retained batches and
new arrivals. Invalid candidate definitions preserve the accepted recipe/view.
When an accepted enrichment fails on later data, diagnostics remain visible.
A dependent filter that cannot evaluate adds no new matches; it never lets
unrelated rows bypass the filter. Clear the dependent filter to inspect those
captured raw rows. Original bytes and stable identities remain unchanged.

This preview supports one named enrichment per view, with string, numeric,
Boolean or null output and bounded display values. List/struct outputs report
errors. Multi-stage editing, command enrichment controls, AI authoring and
multiline grouping remain pending.

Primary validation: UI/app/query/view tests, real extraction/pin/filter/arrival/
invalid-edit/restart/clear PTY, demo PTY, formatting and targeted clippy.

## 010: Independent named views

Historical binary (local bundle removed):
`previews/010-named-views/lvu`.

Press `v` to open the source-view dialog. Alt-B creates a blank view, Alt-D clones
the selected view, and Alt-R renames it. Enter the name and submit. Switch views
with `[` / `]`, the sidebar keyboard controls, or mouse.

Blank views start without filters or enrichment. Clones copy accepted settings,
unfinished drafts, navigation, pins and colors, then execute their own query.
Each view has its own persisted identity. Reopening a source restores its views;
creating or switching views shares capture and does not restart the source command.
Existing single-view workspace state retains its identity.

The current limit is 16 views per source and 128 globally. Duplicate sibling names
are rejected. View deletion, merged multi-source views, and recipe import/export
are not yet available.

Primary validation covers independent search/enrichment, live arrivals, shared
command startup, rename/restore, admission before command launch, delayed restore
fencing, actual PTY workflows, formatting and targeted clippy.

## 011: Ask AI for filters and enrichment

Historical binary (local bundle removed):
`previews/011-ask-ai/lvu`.

Press `A`, then Alt-F for a filter or Alt-E for an enrichment. Describe the desired
result and press Enter. The dialog shows export/agent progress, then the proposed
expression and explanation. Enter explicitly applies through the existing native
editor and compiler; Escape cancels or closes. Invalid expressions preserve the
last accepted view. New user drafts invalidate stale proposals; live arrivals and
navigation do not.

The local Paseo session can inspect fixed filtered/enriched and source-context
Parquet parts, original binary bytes, identities, and a manifest. Export preserves
accepted enrichment batch boundaries, including later batch errors. Snapshot and
session paths are shown in the dialog; session metadata is retained alongside
the manifest under `<capture-dir>/investigations`. One session is reused during
the app run. Shutdown requests remote cancellation and reports incomplete cleanup.

The configured local Paseo daemon must be available. Build the bridge once with
`mise run install:bridge` and `mise run build:bridge` if needed. Defaults are
`codex/gpt-5.6-sol`, `full-access`, and `medium`; override with `LVU_AI_PROVIDER`,
`LVU_AI_MODE`, and `LVU_AI_THINKING` before launch. A bridge failure leaves the
ordinary viewer usable.

This slice supports filters and one-field enrichment proposals. Source proposals,
freeform investigation chat, and session resume controls are still pending.
Ask AI exports at most 50,000 source records, 512 MiB input/output each, and 512
parts; these are complete snapshot limits, not a sample of a larger dataset.
Captured source history counts toward those limits even when a filter is narrow.
Snapshots are retained on disk; a cleanup UI is still pending.

Primary validation: 33 UI state plus two unit tests, 28 app tests, 13 native-view
tests, real-source Ask AI/native-apply/reuse/offline/restart PTY, demo PTY,
formatting, and clippy. The owner also completed a real local-provider proposal;
subsequent lifecycle fixes were checked with deterministic protocol tests.

## 012: Resumable investigations

Historical binary (local bundle removed):
`previews/012-investigations/lvu`.

Press `I`, type a question, and press Enter to export the applied view and start
a separate local Paseo investigation session. Once the response arrives, type
another question and press Enter for a follow-up. The agent has access to the
filtered/enriched Parquet, full captured source context, raw bytes, and definition
manifest. It can choose its own analysis tools; lvu does not implement plotting.

Use Up/Down to select a saved investigation and Enter with empty input to resume
it. Restarting the viewer never sends a prompt automatically. Alt-N starts a new
snapshot; Escape cancels active work or closes the pane. Ask AI definition
assistance and investigation conversations use separate sessions.

Session metadata and snapshots remain under `<capture-dir>/investigations`.
The local conversation pane retains at most 64 messages of 16 KiB each; full
conversation history belongs to Paseo. Saved-session loading shows at most 64
investigations and scans at most 256 directories, reporting truncation or invalid
metadata. Preview011's complete snapshot limits still apply. There is no snapshot
cleanup UI yet. A lost bridge connection or incomplete cancellation is reported;
unresolved cleanup blocks replacement work rather than abandoning session ownership.

Primary validation passed 35 UI state plus two unit tests, 32 app tests, real
export/prompt/follow-up/cancel/restart/resume/offline PTY, demo PTY, formatting and
clippy. The owner also resumed a real local-provider session that identified the
fixture database timeout and its request ID.

## 013: AI source assistance

Historical binary (local bundle removed):
`previews/013-source-ai/lvu`.

Open Add Source with `n` (or start with no sources), then Ctrl-A to switch to
source Ask AI. Describe a source and press Enter. The agent receives bounded,
read-only local project/Docker/process discovery evidence. The returned proposal
shows the exact executable/arguments or shell text, environment overrides,
effective working directory/file path and restart policy in a scrollable review.
Enter explicitly accepts and launches through normal source admission; displaying
the proposal does not execute it. Duplicate definitions reuse existing capture.

Ctrl-A returns to manual entry, Ctrl-D opens discovery, and Escape cancels or
closes. A separate source-assistance session is reused; filter assistance and
investigation sessions remain separate. Context preparation stays single-admission
through cancellation and shutdown. HTTP sources and command restart policies other
than `never` are rejected because the runtime does not support them yet.

Source discovery contexts are retained under the capture directory's investigations
folder, capped at 64 contexts and 2 MiB per manifest. The source-context directory
scan is capped at 1,024 entries. Reused sessions receive a metadata record in each
new context. These limits are explicit; cleanup controls remain pending.

Primary validation: 37 UI state plus two unit tests, 35 app tests, full real-source
PTY including source proposal/review/explicit launch/duplicate reuse/offline and
previous agent workflows, demo PTY, formatting and clippy. A final cancellation
writer regression ensures interrupted serialization cannot retry forever.

## 014: Named reusable recipes

Stable binary:
`previews/014-recipes/lvu`.

Press `r` to browse recipes. Alt-S saves the selected view's accepted configuration
under a name; unfinished drafts are excluded. Alt-I imports a versioned TOML file
into canonical storage for review. Alt-B returns to browsing. Select a recipe and
press Enter to apply it through the native query dispatcher.

Recipes contain literal search, advanced filter, one Polars enrichment, pins and
stable-value coloring. Applying one preserves the target view's identity and
source capture. Any constraint failure keeps the entire prior accepted view;
failed drafts retain diagnostics. Later user presentation edits take precedence.
Delayed persistence responses cannot replace newer dialog input or selection.

Canonical TOML lives under `<capture-dir>/workspace/recipes`. New saves/imports
reject existing names or identities; immutable revision history remains in the
store, while revision editing controls are still pending. The browser lists at
most 128 recipes. Unsupported time policies, command/multiple enrichment stages,
color rules and excessive pins are rejected rather than partially applied.

Primary validation: 40 UI state plus two unit tests, 36 app tests, 21 memory tests,
cross-source recipe/apply/restart real PTY, demo PTY, formatting and clippy.

## 015: Fixed capture-time ranges

Stable binary:
`previews/015-capture-time/lvu`.

Press `t` to edit capture-time bounds. Tab switches start/end, Enter applies,
Alt-A fills ±30 seconds around the record selected when the dialog opened, and
Alt-C clears the window. Use UTC `YYYY-MM-DDTHH:MM:SS[.nnnnnnnnn]Z`.
The interval includes the start and excludes the end.

Windows combine with literal search, advanced filters and enrichment. Each view
remembers its accepted window and unfinished input; recipes carry absolute bounds,
and investigation manifests record the exact applied nanosecond bounds. Invalid
input leaves the accepted view working. Time edits invalidate stale AI proposals
and delayed restoration, while concurrent submitted constraints stay combined.

This version filters physical capture timestamps. Parsed event timestamps,
rolling windows and multiline grouping remain pending. Earlier storage, snapshot,
agent and recipe limits still apply.

Primary validation: 46 UI state plus two unit tests, 36 app tests, 21 memory tests,
13 native-view tests, full real-source capture-time/restart PTY, demo PTY,
formatting and clippy.

## 016: Rolling capture-time ranges

Stable binary:
`previews/016-rolling-time/lvu`.

Press `t`, then Alt-5 for the last five minutes, Alt-M for fifteen minutes, or
Alt-H for the last hour. Alt-C clears the time constraint. Absolute UTC bounds
and the selected-record ±30-second shortcut remain available.

Rolling windows expire records even when sources are idle. They combine with
search, advanced filtering and enrichment. Saved views and recipes retain the
duration policy; reopening resolves it against the current clock. Investigation
exports freeze the applied interval together with its matching records.

Refreshes are requested at one-second cadence. Ticks coalesce behind unfinished
queries and recipe transactions, allowing slow scans to complete. Unchanged
Polars expressions reuse their compiled definitions. Clock refreshes do not
invalidate AI proposals; user definition edits still do.

Current performance limit: a changed rolling interval rescans retained history
in bounded batches. Large histories can make the displayed interval lag the clock;
the one-second cadence is not a completion-latency guarantee. Parsed event time,
a custom duration editor, and multiline grouping remain pending.

Primary validation: 51 UI state plus two unit tests, 37 app tests, 21 memory tests,
14 native-view tests, actual-journal idle expiry and snapshot consistency,
controlled-clock transaction tests, real-source/restart PTY, demo PTY, formatting
and clippy.

## 017: Recognized event-time ranges

Stable binary:
`previews/017-event-time/lvu`.

Press `t`, then Alt-E to use recognized event time or Alt-P for capture time.
Absolute UTC bounds, rolling presets, selected-record ±30 seconds and clear work
with the selected basis. Details show normalized UTC event time, its source field,
capture time and unchanged original fields/raw text.

Recognition inspects top-level JSON/logfmt `timestamp`, `time`, then `ts`, in that
precedence order. Only explicit RFC3339 `Z` or numeric UTC offsets are supported.
Numeric epochs and timezone-less values require explicit interpretation and are
not guessed. Missing/invalid event timestamps do not fall back to capture time;
an active event-time window reports them as unmatched. Clearing the time constraint
keeps these original records available under any remaining filters.

Views, clones and recipes retain the selected basis. Investigation manifests
record it with resolved bounds; Parquet includes nullable event-time nanoseconds.
The shared recognizer reads up to 1 MiB per physical record independently of the
32-field display limit and respects quoted logfmt message contents.

Nested timestamp discovery, numeric epoch interpretation, event-time sorting and
multiline grouping remain pending. Rolling intervals still rescan retained history
in bounded batches, so large captures can lag their requested refresh cadence.

Primary validation: 52 UI state tests, 37 app tests, 21 memory tests, 14 live
integration tests plus three live projection tests, 16 native-view tests,
real-source event-time/restart/clear PTY, demo PTY, formatting and clippy.

## 018: Reversible multiline display grouping

Stable binary:
`previews/018-grouping/lvu`.

Press `m` to edit the per-view continuation regex. The suggested rule is
`^(\s+|Caused by:)`. Enter applies; empty input disables grouping. Enter on the
selected group expands/collapses it, as does clicking the selected group.

Rules use the Rust regex crate over raw bytes, with 16 KiB pattern, 1 MiB compiled
size and nesting-depth limits. Unsupported backreferences/look-around and invalid
rules produce diagnostics while retaining the accepted configuration. Rules and
unfinished drafts persist; recipes save accepted rules.

Grouping is display-only, after physical-record filtering. Matching continuations
whose header was filtered out remain visible as orphan groups. Streams and sources
stay separate. Groups split at 64 lines or 64 KiB; an oversized individual record
is preserved alone with an explicit label. Live groups can grow while their leading
stable selection remains anchored.

Worker-owned projections let groups render even when the raw cache holds fewer
rows than a group. Each constituent text projection is limited to 4 KiB and its
base projection to 8 KiB; derived presentation fields can add bounded overhead.
Grouping projections consume the membership budget. Original bytes, stable IDs
and investigation snapshots remain physical and unchanged. Disable grouping for
ordinary individual-record inspection. Semantic multiline enrichment is not yet
implemented.

Primary validation: 54 UI tests, 37 app tests, 14 live integration tests, 21 memory
tests, 20 native-view integration tests plus two view unit tests, full real-source
grouping/filter/orphan/restart PTY, demo PTY, formatting and clippy.

## 019: Storage usage and disposable-index cleanup

Stable binary:
`previews/019-storage/lvu`.

Press `S` to inspect capture, derived-index, workspace/recipe and investigation
storage. The browser also reports row-cache and query-membership usage/limits and
the per-source derived-index cap. These budgets are not a process-wide RSS limit.

Press `c` to review reclaimable indexes, then `c` again to clean those reviewed
candidates. Cleanup verifies the index format, checksums, source identity and file
revision under ownership locks. Registered, locked, changed, newly appearing,
unknown and unsupported artifacts are preserved. Raw journals, capture cursors,
catalogs, SQLite/TOML settings and investigation data are excluded from cleanup.

Cleanup is supported on Linux using retained directory descriptors and relative
filesystem operations. Other platforms refuse cleanup. Index validation is capped
at 16 MiB per artifact; larger indexes are labeled unverified and preserved.
Scans are bounded to 512 entries per directory, 4,096 files, 1,024 directories,
depth four and 16 reported errors. Partial results and errors are visible.
Cancellation is checked during validation and before mutation; shutdown reports
workers that fail to settle within three seconds.

Primary validation: 55 UI tests, 39 app tests, five live unit tests, 14 live
integration tests, ownership/replacement and sentinel-preservation regressions,
real-source storage cleanup/continued-capture PTY, demo PTY, formatting and clippy.

## 020: Searchable command palette

Stable binary: `previews/020-palette/lvu`.

Press Ctrl-P to open the palette. Type to search operation names, categories and
aliases; prefix and fuzzy matching rank results. Up/Down selects, Tab completes
the selected operation name, and Enter executes it through the normal handler.
Shortcuts are shown alongside operation names. Context-specific operations remain
listed with a prerequisite explanation when unavailable.

Escape or Ctrl-P closes the overlay without changing the underlying editor draft.
Mouse scrolling/selection and bounded paste work inside the palette. Background
capture and query progress continue. Storage cleanup retains its existing review
and confirmation requirements. Main panels and their contextual actions are listed;
the catalog grows with new features.

Primary validation: 11 palette tests, 55 UI state tests, 39 app tests, full real-source
PTY, dedicated live-source palette/search/autocomplete/execute/paste/disabled-action/
editor-restoration/arrival PTY, existing demo PTY, formatting and clippy.

## 021: Similar-source recipe suggestions

Stable binary: `previews/021-suggestions/lvu`.

Open `r` to review ranked recipes and their evidence. Alt-G refreshes suggestions,
`x` records a rejection, and Enter applies through native validation. Alt-A asks
the local agent to adapt a selected recipe against a fixed data snapshot. The
review explicitly limits adaptation to the advanced filter; other recipe settings
remain intact. New operations are also listed in Ctrl-P.

Ranking uses source/project/command metadata, sampled field names, lexical display
hints, usage and outcomes. Display text is not treated as native schema evidence.
Missing fields are labeled as not observed in sampled visible rows; empty samples
do not erase prior evidence. Stale proposals and failed validation preserve the
working view and do not record acceptance.

Primary validation: 58 UI tests, 11 palette tests, 39 app tests, 22 memory tests,
full real-source suggestion/adaptation/restart PTY, live palette PTY, formatting
and clippy. Full configuration adaptation and multi-stage enrichment remain ahead.

## 022: Heart startup/activity and expression completion

Stable binary: `previews/022-heart-completion/lvu`.

Startup shows a coral pixel heart with lvu / love you for at most 600 ms. Any
key, mouse input or paste dismisses it and continues through normal input handling.
Capture/query ticks continue underneath. The footer pulses at eight frames/second
for pending query/agent work or observed view updates; idle/error remain readable
and no percentage is invented. The heart is suppressed below 60 columns to keep
core status visible. View switching alone does not count as background work.

Set LVU_REDUCED_MOTION=1 for static accents, LVU_NO_DELIGHT=1 to disable them,
or LVU_ASCII=1 for an ASCII heart. These switches use environment-variable presence.

In `p` or `e`, Tab opens a bounded field picker; another Tab switches to sampled
lexical string values. Arrows/mouse select, Enter inserts escaped Python text,
and Escape closes. Completion appends at the editor end and never submits a query.
It samples loaded visible rows; it is not full Python API/context-aware completion.
Literal search remains literal.

Validation includes module/TestBackend checks, actual startup/first-key/expiry/
arrival/reduced-motion/disabled/resize PTY and integrated expression-completion
filter/enrichment/restart workflows. CLI positional files and stdin are being
implemented separately and are not included in this preview.

## 023: Positional files and redirected stdin

`mise run preview` opens this version. Stable binary:
`previews/023-stdin/lvu`.

Pass file paths directly, keep --file for compatibility, or use repeatable
--command/-c with shell text. -- terminates options for dash-prefixed paths.
Redirected stdin is automatically added; '-' and --stdin select it explicitly.
Duplicate stdin requests and terminal stdin requests are rejected. File paths
retain native OS bytes, including non-UTF-8 Unix names.

Linux pipe capture reopens the endpoint with isolated nonblocking flags. Regular
files and /dev/null also work; other devices are rejected. Keyboard input uses
Crossterm's controlling-terminal reader, independently of fd0 log data. Startup
without a controlling terminal fails before acquisition. EOF leaves the view open;
quitting before pipe EOF cancels capture with bounded cleanup. Each stdin run has
its own identity, and partial bytes are retained before read errors.

Validation includes integrated core/runtime/app tests, actual controlling-PTY
mixed source/input/EOF/early-quit tests, parent-held descriptor flag preservation,
device policy and named FIFO with no writer, plus full existing real-source PTY.
XDG settings/themes remain under implementation; this version still uses the
existing capture-directory defaults unless --capture-dir is supplied.

## 024: Case conversion and discovery relevance

Historical executable: `previews/024-case-conversion/lvu`.
Run with `mise run preview app.log`; task help is now available through
`mise run preview --help`.

Enrichment accepts `upper = pl.col("raw").str.to_uppercase()` and lowercase.
Unicode expansion and null propagation are verified in Python and Rust, including
one-row batch partitions. Generic unsupported-operation errors no longer claim
that an omitted method necessarily depends on other rows.

Automatic discovery excludes database, SQLite sidecar, lock and known lvu
artifacts while retaining explicit tee/stdout log destinations. Manual file
addition remains unrestricted. Discovery list scrolling/highlighting, new dialog
surfaces/cursors, standard themes/settings, persistent title art, and brain-emoji
interface labels are still under UI integration and are not included here.

Primary validation: 23 Python tests; 26 query/host/library tests; 80 differential
values/nulls/dtypes; 15 discovery tests; native query clippy; formatting; actual
Unicode uppercase enrichment PTY and complete real-source PTY workflows.

## 025: XDG settings, themes and consistent dialogs

Historical executable: `previews/025-settings-dialogs/lvu`.
Open settings with `,` or Ctrl-P. Model choices, appearance and cache bounds map
to `$XDG_CONFIG_HOME/lvu/settings.toml` (fallback `~/.config/lvu/settings.toml`).
Themes: Terminal, Love Dark, Love Light, Dracula, Nord and Gruvbox Dark. Theme
preview applies immediately; cache bounds require restart. Shared derived-cache
accounting enforces the aggregate cap, and storage inspection uses the actual
XDG cache root. Durable captures and exports are preserved by cache cleanup.

Dialogs have padded input surfaces, visible cursors and separate colored shortcut
footers. Discovery uses highlighted single-row candidates with bounded details.
Interface assistance is labeled 🧠, with an Agent fallback in ASCII mode.
The shaded pixel-art LOVE YOU LOG TIME title stays until Escape, consumes other
input, and is skipped for every CLI source (including empty/slow sources).

Global derived indexes are bound to backing journal identity, so separate capture
roots cannot accidentally reuse one another's offsets. Legacy indexes remain
recognizable for safe cleanup.

Validation: 97 UI tests, 53 app/settings tests, 29 live tests, 25 memory tests;
clippy and formatting; full real-source PTY plus settings, title, palette, stdin,
demo and delight workflows. The PTY parser now handles orphaned wide-character
cells without crashing; the exact display case has an independent regression.

Gzip decoding and ordered iterative enrichment are still being integrated and
are not in this build. Enrichment remains one active definition here; failed
edits retain the last working definition. Existing Linux and source limitations
remain. No raw data is automatically migrated or deleted.

## 026: iterative extraction, search and timestamp assistance

Historical executable: `previews/026-search-extraction/lvu`.
Source: `d787c7b015a3cea053eb39d3f57c150d5991e678`.
SHA-256: `a5e0ee1aed54a330f977c96e8a3d39353a1c91815bfa1d61642519c3a18a7a56`.
Run `mise run preview app.log` or `mise run preview archive.log.gz`.

- `/`: 300 ms debounced search. `timeout` searches raw literally, `level: error`
  searches a field, `/timeout|refused/i` uses regex, `message: /^timeout/` targets
  a field with regex, and `pl.col('status') >= 500` accepts an explicit predicate.
  Use a colon followed by a space for field selectors. Plain literals ignore case;
  regexes honor optional `i`, `m`, `s` flags. Clear reliably restores raw membership
  while keeping any other accepted constraints; invalid drafts retain diagnostics.
- `e`: ordered, accumulated enrichment stages. `/id=(?P<request_id>\S+)/`
  extracts named fields with Polars. Later expressions can use them. Alt-A adds,
  Alt-E edits, Alt-R removes, Alt-J/K selects. Empty Add never removes prior stages.
  Failed changes preserve the whole previous chain. Recipes/restarts preserve IDs,
  original editable definitions and unfinished drafts.
- Left/Right scroll event text horizontally; `0` resets. Metadata stays anchored.
- Gzip is detected by magic bytes, supports concatenated members, preserves exact
  decompressed bytes, and reopens unchanged archives without duplicate records.
- `t`, then Alt-T: review an editable timestamp-recognition prompt. It asks for a
  `timestamp_utc` enrichment using explicit formats and UTC RFC3339 microseconds,
  with no guessed timezone/year/epoch unit. This creates a derived column; the
  existing recognized-event time basis still reads original raw timestamp fields.
- Glossy reflected red pixel heart above gold bitmap lettering; footer heart with
  a two-beat pulse trace. Source-less title remains until Escape; CLI sources bypass
  it. Reduced motion and idle indicators remain static.

Limits: regex/field forms use projected scalar fields; arbitrary Polars predicates
still need the locked Python compiler. Gzip archives are static, not followed;
changed archives need a fresh capture identity (for example a new `--capture-dir`).
Gzip stdin is not included. Horizontal positions are per-view for the current run.
Cache caps still require restart; no raw data is automatically evicted.

## 027: Heart-only pulse and Luna default

Historical executable: `previews/027-heart-luna/lvu`.
Source commit: `c56e3f6`.
SHA-256: `532ff9138097515d603ae10543febd9202c2f85c669552ef07baac777e7c6fcb`.

New configurations default to `codex/gpt-5.6-luna`; explicit saved settings and
environment overrides remain authoritative. The footer has no adjoining ECG
glyphs: the heart alternates filled/outline in a double-beat rhythm while active,
with fixed label placement and static idle/error/reduced-motion states. Terminal
cells do not smoothly scale, so this is a filled/outline pulse, not pixel zooming.

Validation: 10 delight rendering tests, 11 settings tests, lvu/app clippy with
warnings denied, formatting, real delight/settings PTYs and a settings PTY of
the copied preview binary. All passed. No new live-model request was issued.
Remaining product work and known issues are tracked in [TODO.md](../TODO.md).

## 028: Extracted time and quoted field search

Historical executable: `previews/028-extracted-time/lvu`.
Source commit: `aac0bafc7010e9b9dfd0e42c5d4ad9fed61bdea5`.
SHA-256: `00782af43d1b00e4afc8bf58d608df1f3e2a0eff204343ad336374aa448d9a41`.

Time (`t`) now offers **Alt-U** to use an accepted `timestamp_utc` enrichment.
The output must be a UTC RFC3339 string. Missing/invalid values remain unmatched;
there is no raw/capture-time fallback. Alt-A centers on the selected derived time.
The basis survives restart and recipe round trips; snapshots retain exact selected
timestamps in `_lvu_selected_time_unix_nanos` and identify the basis in the manifest.
Removing the required enrichment while a time filter depends on it is rejected
without changing the working view. Arbitrary timestamp-field names remain future work.

Search accepts JSON-quoted names, such as `"field name": error` or
`"field name": /^ready$/`. Use `\/var/log` for literal leading-slash text.

Validation: lvu/app/memory/query/view tests, relevant clippy with warnings denied,
formatting and actual extracted-time/search PTYs passed. The copied preview passed
extracted-time apply, arrivals, selected anchoring, restart, clear and restoration.
Concurrent search/gzip runs and four parallel gzip suites (32 opens/reopens) passed.
The previously observed intermittent empty reopen remains unresolved in TODO.md.

## 029: Raw context and capture controls

`previews/029-context-controls/lvu` is a historical path; the local bundle was removed. Source: `af4fb55`.

- `o` inspects neighboring raw records without changing the filter. Arrows, page
  keys and mouse scroll; `g` returns to the fixed anchor; Escape closes.
- `Alt-S` stops the selected shared capture. `Alt-R` restarts files/commands while
  keeping views and filters. Files resume without duplicates; commands run again
  only on request. Stdin restart is refused before affecting its pipeline.
- Luna timestamp assistance now receives the exact proposal schema, single-
  expression instructions and a pinned Parquet reader. Actual UTC conversion
  passed through model proposal and native application.

Rust UI/app/live/view tests, clippy and both copied-binary PTYs passed. The bridge
passed 43 tests/typecheck/build and a retained actual-provider proof. Earlier
previews remain. Open limits are tracked in TODO.md; this release does not add
HTTP acquisition, automatic command restart, correlation or bookmarks.

## 030: Bookmarks and portable recipes

`previews/030-bookmarks-recipes/lvu` is a historical path; the local bundle was removed. Source: `076b9f8`.

- `b` bookmarks the selected stable record; `B` opens bookmarks and notes.
  Enter opens raw context even if a filter hides the record; Alt-E edits a note
  and Alt-D removes the bookmark. Working-state persistence retains both.
- `r`, Alt-E exports the selected immutable recipe revision to a new TOML file.
  Existing destinations are preserved; import/apply in another workspace remains
  an explicit action.
- Bounded journal read-ahead reduces system calls without changing journal bytes
  or page offsets. Comparable wall times were similar; see performance.md.

The copied binary passed bookmark restart/hidden-context/removal and portable
recipe export/import/apply PTYs. Rust suites, clippy, gzip/reopen and source-control
PTYs passed before copying. SHA256:
`52169a87ee7a61c97850526399873a05df60fb57a006ca6c39e68e0e20a67cbd`.
Prior previews remain. TODO.md tracks unresolved reopen investigation and remaining
product work; this checkpoint does not claim SSH or sustained-throughput acceptance.

## 031: Recipe history and ordered adaptation

`previews/031-recipe-workflows/lvu` is a historical path; the local bundle was removed. Source: `b4a3526`.

- Recipes: Alt-H reviews up to 100 immutable revisions; Alt-U explicitly saves
  accepted settings as a new revision. Stale updates are rejected. Applying or
  exporting an old revision does not change the current recipe pointer.
- 🧠 recipe adaptation can propose the complete ordered enrichment chain and
  advanced filter. Schema/host checks precede native atomic validation. Omitted
  chains retain reviewed stages; empty chains explicitly propose clearing them.
- PgUp/PgDn/Home inspect long proposals and all stage sources before Apply.

Copied-binary history and ordered-adaptation PTYs passed, including restart.
Context/bookmarks passed through an isolated authenticated localhost SSH daemon
with resize and terminal restoration. UI/app/memory checks, clippy, 44 bridge
tests and an actual Luna two-stage proposal/native application passed. SHA256:
`6dd31015ccadc8ccd91a6a886d24b022730887ea60a011e101d5d4d189aaa4ec`.
Earlier previews remain. Loopback SSH does not establish behavior under remote
network latency; sustained workloads and other open work remain in TODO.md.


## 032: Inspect, copy and combine sources

`previews/032-inspect-and-copy/lvu` is a historical path; the local bundle was removed. Source: `7ca1e2d`.
SHA-256: `357cb7decc7df66f9dbcc4170b3391b368d7fbb09ebdd3007227edacd44cbf0e`.

- Drag visible text, Ctrl-C sends it to the terminal clipboard through OSC 52.
  Escape dismisses the dialog and clears selection; drag again to copy the log
  beneath it. Clipboard support must be enabled in the terminal/multiplexer.
  Copy is bounded to 64 KiB and includes only displayed text.
- The enrichment dialog separates saved steps, multiline input, validation and
  raw/derived samples. `str.replace` and `str.replace_all` execute through the
  Python compiler/native Rust path. Existing successful steps remain intact.
- Timestamp assistance prefers the actual structured columns and values, without
  assuming an input field name. A live Luna proof used `observed_at` directly and
  normalized its offset to UTC. Fixed sample coverage remains future work.
- `v`, Alt-M edits ordered sources of a view; Space includes/excludes, Alt-Up/Down
  reorders, Enter applies. Sources must already be open. Rows display their source
  and order by source position then sequence. Invalid changes preserve the view.
  Clone and restart retain memberships. Reopening waits for missing sources to be
  explicitly opened; remembered commands never start automatically.

The workspace DB moves to schema v3 to mark source membership. Existing state
migrates; older builds refuse opening that future schema. An owning source remains
required, and a source with bookmarks cannot be removed until those bookmarks are
explicitly removed. This is not event-time interleaving.

Combined Rust tests/clippy, Python tests, bridge typecheck/tests/build, full real
and demo PTYs passed. The copied binary passed selection/replacement, ordered
chain/restart and merged-view/deferred-restore PTYs. The intermittent plain-file
empty reopen remains unconfirmed; other remaining work is tracked in TODO.md.


## 033: Dialog hierarchy, animated artwork and broader expressions

`previews/033-dialogs-and-heartbeat/lvu` is a historical path; the local bundle was removed. Source: `d969c4c9615fe74b4f19725bb7a1e8d889794bd3`.
SHA-256: `8fcdd5638b59cb8d8a6e9e0db189c5852e7e88ba3ab25a9c93ef169289a35703`.

Search separates editable input, muted examples, applied state and actions. Help
uses aligned themed groups and modal scrolling. Dialog footers contain actions;
Discovery and Storage diagnostics have separate inspectable areas. Narrow dialogs
keep editable rows and indicate when more space is needed.

The supplied title animation uses original 110 ms frame timing and any-key consumed
dismissal; CLI sources skip it. The corner heart uses the supplied four-frame
sprite and stays quiet at rest. Clipboard selections remain within modal/pane
bounds. Full redraw no longer queries the cursor or consumes queued Escape input.

Polars IR metadata permits broader row-separable expressions, including Datetime
constructors, while retaining row identity, ordering and batch-independence checks.
Timestamp manifests specify distributed sample offsets (128/source, 512 total);
proposal schemas bind exact revisions. Requested inspection is not an I/O cap.

Built on main to retain `/home/venky/dev/lvu` helper/bridge paths. The copied binary
passed Search, title/CLI bypass, clipboard, redraw, ordered enrichment and full
real-source PTYs. Main-built demo passed color Help, heartbeat and normal/panic
restoration. Prior previews remain unchanged. This local binary still depends on
the checkout's helper/bridge and pinned runtime tools.


## 034: Reviewed command enrichment and smaller heartbeat

`previews/034-command-enrichment/lvu` is a historical path; the local bundle was removed. Source: `7bbe24f236ce026262f32954d65766f7992fcf07`.
SHA-256: `b68c3534064ff8863e6076a92b3e8110c49c76005e9c1ba63016eacf77f1d07e`.

One optional command step follows the accepted native enrichment chain. Open `e`,
Alt-C; Ctrl-S saves, Ctrl-R prepares fixed-input review, Enter explicitly runs.
Saving/restoring never executes a command. Durable attempts prevent repeated
delivery, typed results appear in scrollable Details, and failed candidates retain
previous results. See [command enrichment](command-enrichment.md) for the protocol
and bounds. **Workspace schema v4 cannot be opened by preview033 or older.**

The corner heart is 7×4 cells, approximately half its prior width/height. Its
transparent pixels no longer use the terminal's default foreground, removing
white bands; the white heartbeat pulse remains. Four sidebar rows are reserved.
Command save/prepare/run/result publication animate the heart without labels.
Simultaneous resize and Escape now use Crossterm's level-triggered TTY backend.

Main rendering tests, geometry and UI/app clippy passed. The final copied binary
passed command, clipboard and 24 simultaneous resize/Escape cycles; final demo
heartbeat transparency/animation passed. Before the final artwork-only changes,
the corrected backend candidate also passed stdin, startup, full real-source and
normal/panic demo acceptance. The identical resize regression failed on the old
backend; queued-byte inspection confirmed unread Escape. Previous previews remain.


## 035: Quiet workspace status

`previews/035-quiet-status/lvu` is retained as a prior preview. Source: `35cac018cabacccc6b7d73b737d466d51d9e7715`.
SHA-256: `c511f1e9605b9cf13afd4505c47bac3394a6b037e16e32dca634ba2a6aa9144d`.

The main status keeps follow/history, query state and visible row counts, with
only `? help` as a shortcut hint. All bindings remain in Help and the command
palette. It includes034's half-size transparent heart and command enrichment;
schema-v4 compatibility limits still apply.

101 UI-state tests and UI clippy passed. Copied-binary command, simultaneous
resize/Escape and focused status/Help/restoration PTYs passed. Prior previews
remain unchanged.


## 036: Aligned corner and readable Search

`previews/036-aligned-layout/lvu` remains available. Source: `fe0db13a8db96d907d7a351ae1684b6bfc658358`.
SHA-256: `ed27f410c829f9b5379e5152e7b89c4e76dd73d02de0ca6c7b4598e8b16532c4`.

The 5×3 heart is centered in the full sidebar width. Main status begins at the
log pane, so it no longer overlaps the left panel. Search appears once as a title;
input is followed by applied status, with readable examples below. Shortcuts remain
in Help, with only ? help in the main status. All035 functionality and schema-v4
compatibility limits remain.

Full UI tests/clippy and geometry checks passed. The copied binary passed color
Search/input/applied-state checks, simultaneous resize/Escape and clipboard-boundary
PTYs. The main-built demo passed the smaller heartbeat/transparency PTY. Previous
previews remain unchanged.


## 037: Time form, navigation and in-pane heartbeat

`previews/037-time-navigation/lvu` is the current preview.
Source: `b6221f5b6f7ef81e6cbfbb68661db01fc2a7aaba`.
SHA-256: `b14e7953a4a8e20ad1699b293784cb26d1804cc1f0b03629ae62cd191153efbf`.

Time now has prefilled date/time/timezone segments, staged dropdown choices and
focusable Apply/Clear/Recognize actions. Partial drafts persist independently of
accepted bounds. Numeric offsets normalize without losing nanosecond precision;
Around selected uses a frozen anchor. PgUp/PgDn/Home/End bindings are removed in
favor of focused navigation, and the empty Search shortcut strip is gone. The
5x3 heartbeat sits inside the bottom-left of the full-height sidebar border.

Main validation: 270 Rust tests (154 UI,69 app,11 settings,36 memory), all-target
clippy and formatting pass. Copied app PTYs pass extracted Time offsets/narrow
editing/arrivals/anchor/restart/Clear, Search, command Details, clipboard boundaries
and resize/Escape regression. Main demo heart and color Help PTYs pass. Logs are
`/tmp/lvu-preview037-*.log`.

Schema-v4 and checkout-resource limitations remain. No JSON syntax highlighting,
field correlation or new assistance preparation/lifecycle feature is included.
Published artifacts were not overwritten; local retention is listed above. No user capture database was used in acceptance.


## 038: JSON colors and Fields feedback

`previews/038-json-fields/lvu` adds automatic JSON key/value coloring, readable
empty/unavailable Event fields feedback and the requested Help cleanup.
Source: `08e21233e00ce947195a6e5366081b63ad9e05d1`.
SHA256: `146589e29a2c8d62d3dfab1d4cfd06343481f0fb4525fc80b927f61339076043`.

241 Rust tests pass (UI161, app69, settings11), along with UI/app all-target
clippy and formatting. Copied real-app PTYs pass JSON file capture, decoded-key
color identity, exact lexical clipboard contents, horizontal pan, empty Fields
and raw-context return, cross-modal copy, and resize/Escape. Color Help PTY passes.
Truecolor is verified; indexed-color conversion and contrast against an unknown
Terminal-theme background are not guaranteed. Raw data is unchanged.

No assistance/export, correlation, editor-controls or layered-dismissal changes
are included. Runtime helpers still require the main checkout. Schema remains v4.


## 039: Time and enrichment forms

`previews/039-time-editor/lvu` groups Time bounds, adds staged UTC/offset choices,
shows explicit enrichment/command action buttons, and supports shared Ctrl-A/E/K
and arrow editing. Discovery and other status/help text use readable foregrounds.
Source: `116a947fdd0ee097c91fb4f4c940def516f5aad7`.
SHA256: `a41da0d2a23c34cab74e53b39cf8ebc74df61c369bfd18ffcca4e7a8762e700f`.

257 Rust tests, UI/app clippy and formatting pass. Copied actual-app editor,
extracted Time, full external-command, Discovery contrast, clipboard and
resize/Escape PTYs pass. One initial Time shutdown returned nonzero without
captured exit details. It remains unexplained; isolated and concurrent reruns
pass and the harness now retains exit code/output. This is not claimed fixed.

The wider dialog audit, layered dismissal, assistance changes and multi-instance
capture design remain separate. Runtime checkout dependency and schema v4 remain.


## 040: Settings form

`previews/040-settings-form/lvu` adds grouped Settings controls, a theme dropdown,
explicit Save, separate status/details and bounded overflow navigation. Late save
acknowledgments preserve a newer dialog's draft and preview.
Source: `e26c2c3f7b2efb413db55d998f2b2ae32f170a6e`.
SHA256: `3478b7ec1b1e21f24c360d3f7d2b6fc15d43e0073ea490adb4e812e37b635296`.

259 main Rust tests, UI/app clippy and formatting pass. Copied-app Settings tests
cover separate mouse/keyboard saves, live preview, restart, resize focus, truecolor
and restoration; actual-file editor controls also pass. Evidence:
`/tmp/lvu-settings-proof-5qm3cuam`, logs `/tmp/lvu-preview040-*.log`.
The implementer's earlier Enter-after-resize failure has no established timing
cause; current tests assert completed focus frames before acting, not a product
fix for that earlier observation. The unexplained039 Time shutdown remains open.

Assistance and layered q/Escape changes are not included. Runtime checkout
resources and workspace schema v4 remain required.


## 041: Inline assistance and managed helper sessions

Executable: `previews/041-inline-assistance/lvu`.
Source: `08e60299345a236d36d12bbb8799b94faf7c0827`.
SHA256: `efcdbf43306398aef3491d834570da16782e10f1eeb34be05114b0d3ef398f6a`.

Short Ask requests receive a bounded typed context directly in the prompt,
including schema, sampled values, provenance and coverage. No full Parquet export
is made for that path. Ephemeral Ask/source helpers use a stable lvu workspace,
retain activity and confirm archival; investigations remain resumable. Full
snapshots use schema-reference manifests and compatible Parquet row-group packing.
Source completion also keeps its caret at the end of the completed path.

Parent validation:406 Rust tests (two opt-in benchmarks ignored),80 bridge tests,
62 Python tests, all-target clippy, typecheck/build and formatting pass. The full
copied real-source suite passes, including Ask/source/recipes, investigation
resume/offline, path completion, filtering/enrichment, Time, grouping and storage.
The equivalent supervisor live Luna Time proof used6749 bytes of inline context,
applied UTC strings and confirmed owned archival; no tool calls were recorded.
That live proof preceded the final inline-first prompt wording and is not a
latency benchmark. See the work ledger for exact logs and retained failures.

One full-run Ask restart showed blank rows despite ready one-record membership.
The unchanged preserved full rerun and12 consecutive restarts pass, but do not
explain that failure. It remains open, along with the previous Time shutdown and
Settings resize observations. Additional inspection after short-context omissions
is unfinished; bounded full-snapshot decoding is validated on Linux. The new Ask
form, layered dismissal and correlation are separate unpublished work. Runtime
still depends on helper resources in this checkout. SQLite remains schema v4.


## 042: One-layer dismissal and visible path completion

Executable: `previews/042-layered-dismissal/lvu`.
Source: `06beb1adacacbc0b39e057c57df5468647440024`.
SHA256: `7d2f88a72285a14e2e6f91acf8bee6bb25a6221fa20320a525cce82a8ff0bf50`.

q/Esc dismiss the innermost selection, completion, dropdown or dialog before
workspace quit. Focused Details closes and returns to Logs. q remains literal
in text inputs without an overlaid completion layer. Pending command publication
retains its existing close-only ownership. File mode also has a visible Complete
path action, preserving keyboard/mouse completion and Unicode continuation.

Parent validation:284 UI/app/settings tests, lvu/app all-target clippy, formatting
and app/demo builds pass. Actual copied app and matching demo exercise text input,
selection, completion, Time dropdown, Context return, Details and workspace exit;
terminal modes restore. Copied app path completion passes keyboard/mouse, Unicode
and ingestion checks. Proofs `/tmp/lvu-layered-dismissal-proof-b45oq2uc` (actual app)
and `/tmp/lvu-layered-dismissal-proof-wu__ucio` (demo); logs
`/tmp/lvu-preview042-{tests,clippy,build,real,demo,completion}.log`.

The shared visual redesign and background capture worker are not included. Prior
unexplained Time shutdown, Settings resize and blank-row observations remain open.
Runtime uses checkout helper resources; SQLite remains schema v4.
