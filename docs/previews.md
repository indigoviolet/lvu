# Runnable development previews

Previews are published after useful independently tested checkpoints. User feedback
is optional and never gates ongoing implementation. Each publication has a stable
versioned local binary and a manifest with its source commit, checksum, checks and
known limitations. Previous binaries remain available while development continues.
`previews/latest` points to the most recent tested version; `mise run preview` runs it.
Binaries are local build artifacts and are not committed to Git.

## 001: TUI demo

This older version remains available at the stable executable below.
Stable executable: `previews/001-tui-demo/lvu --demo`.
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

This older search demo remains available at the stable executable below.
Stable executable: `previews/002-text-search/lvu --demo`.
Implementation: `e07bd8d` (owner `5a3974c`).

Press `/` and type `request 05`, `café`, or `late fixture` to narrow demo logs.
Search updates after 120 ms of idle typing. Punctuation is literal; case matching
uses Unicode lowercase. Press Escape to return to navigation. Reopen `/` and
backspace to empty to restore all rows. `a` supplies synthetic arrivals that obey
the active search. Views retain independent search state.

`p` opens the separate advanced Polars editor; execution remains unwired in this
preview. Real sources are still being integrated. Preview 001 remains available.

Primary reran 13 TUI tests and the actual PTY workflow after integration, including
search/no-match/clear/arrival behavior and normal/panic terminal restoration.

## Upcoming checkpoints

- Working native Polars filtering and additive enrichment.
- Local source discovery and restored view memory.
- AI-assisted definitions and investigation handoff.

Each checkpoint will document its actual supported behavior and a run command.

## 003: Real local sources

This version opens the Add source dialog. Tab switches file/command mode;
Enter starts capture. Stable executable: `previews/003-real-sources/lvu`.

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

This older version remains available. Stable binary:
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

This older version remains available at `previews/005-real-search/lvu`.

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

This older version remains available at `previews/006-path-completion/lvu`.

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

This older version remains available at `previews/007-working-memory/lvu`.

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

This older version remains available at `previews/008-field-presentation/lvu`.

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

`mise run preview` opens this version. Stable binary:
`previews/009-polars-enrichment/lvu`.

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
