# lvu-app composition

`lvu-app` is the real-source executable. See the current
[architecture map](../../docs/architecture.md) for cross-component ownership. It composes the accepted
capture runtime with the live journal row provider and the Ratatui UI without
duplicating their acquisition, storage, or paging logic.

```text
lvu-app --capture-dir ./captures --file ./service.log
lvu-app --capture-dir ./captures --command 'make serve'
```

`--file` and `--command` are repeatable. Commands are explicitly executed as
`sh -c` with the application's current directory recorded in the source
definition. With no source arguments and no previous session to resume,
`lvu-app` shows the title until any key, then a bounded Add source dialog.
File suggestions appear while typing, without invoking a shell; Up/Down select
one and Enter opens it or enters a directory. Alt-F selects file input and
Alt-C selects command input. Completion expands `~/` for enumeration while
preserving the typed tilde form, and command completion is intentionally disabled.

Press Ctrl-D in the source dialog to run a bounded, asynchronous discovery scan.
The scan combines Docker, Linux `/proc`, and the current project providers;
remembered sources are loaded from persisted memory and require explicit launch. Type to filter
the returned candidate list, use the arrow keys to select, and press Enter to
explicitly start it. Discovery never starts a candidate autonomously. Ctrl-R
cancels any active generation and starts a fresh scan. Provider failures,
partial limits, timeouts, cancellation, and an empty result remain visible.

## Integration boundary

The UI carries an opaque candidate fingerprint while `lvu-app` retains the full
`DiscoveryCandidate`. Selection passes its authoritative `SourceDefinition`
directly to `SourceManager`; it does not reconstruct paths or Docker commands.
Normal source admission, duplicate reuse, registration rollback, and shutdown
remain shared with manual sources. The memory composition populates `ProjectConfig::recent_sources`.

The application registers the same source handles and raw views with
`NativeViewAdapter`. Literal searches execute entirely in Rust and incrementally
refresh as capture advances. Advanced Polars text is compiled lazily by the
pinned `python/` helper (`mise exec -- uv run` in a checkout, a plain package on
`PYTHONPATH` in a packaged archive; `resources.rs` decides); the helper is never
started for literal-only searches. Both constraints are applied together with AND, while
invalid advanced drafts leave the last accepted view and its live refresh active.
The terminal uses the adapter's cloneable row handle and passes its mutable query
half through the composition tick, avoiding duplicate mutable ownership.

Press `e` to add ordered enrichment definitions: named Polars expressions or
`/regex/` with named capture groups. Successful additions retain earlier stages;
later stages may use their outputs. Invalid add/edit/remove operations preserve
the complete accepted chain and membership. Recipes and working state retain
stable stage IDs and editable source text. Optional assistance proposals,
multiline grouping and persistence are composed in the application. Command
enrichment is composed here too, through `command_controller.rs` and its
siblings; see `docs/command-enrichment.md`.

Press `d` to show the selected event's original raw text and its recognized
JSON/logfmt fields, with nested JSON as a tree that opens in place. Press `i`
for the Fields dialog: the same tree with a Value pane, where Space or Enter
pins a column and the action row's Pin, Filter, Exclude, Color, Fold and
Correlate act on the selected value (`c` still toggles colour). Pins and the
colour field are stored with the working view. Projection is limited to
visible live-adapter rows; runtime fragments remain physical records.

The root workspace includes `lvu-view` and its locked dependencies.

Press `v` on a selected source view to manage independent named views. Alt-B
creates a blank view, Alt-C clones accepted settings plus editor drafts and
presentation, and Alt-R renames the selected view (Alt-D still clones,
unlisted). All views reuse the source's
single capture handle and retain independent filters, enrichment, navigation,
pins, and colors. The application admits at most 16 views per source and 128
views globally before registering query state.

Alt-S in the view dialog edits ordered source membership (Alt-M still works,
unlisted). Space includes/excludes
an already-open source; Alt-Up/Down changes its position. Enter atomically applies
the existing filters/enrichments to the proposed sources. Failed or superseded
changes preserve the prior view. Rows are ordered by source position and then
record sequence, with a source column; clock-based interleaving is not implied.
Clones retain membership. The owning source stays included; remove a source's
bookmarks explicitly before excluding it. No capture is launched by a view edit.

Working membership persists in the workspace database (since schema v3; the
current version and its history are in `docs/previews.md`); legacy
single-source state migrates without changing settings. On reopen, a merged
view waits until every referenced source is explicitly opened. Its stored
state is retained while waiting, and remembered commands never start
automatically. The view becomes available when its sources are open. An older
build refuses a newer schema at startup.
Recipes store reusable processing/presentation settings and retain the target
view's source membership when applied.


Drag visible text in a dialog or the log pane and press Ctrl-C to copy. This uses
OSC 52 to request the terminal clipboard; the terminal/multiplexer must support
and enable it. A selection freezes its displayed frame while acquisition continues.
Escape dismisses the dialog and clears selection; a subsequent drag selects the
newly visible log text. Copy is limited to 64 KiB and includes visible text only,
not clipped/off-screen data. Without a selection, Ctrl-C retains its quit action.

The enrichment editor separates saved steps, a multiline expression, validation
status and input/output samples on larger terminals; compact terminals retain a
bounded layout. `str.replace` and `str.replace_all` are supported by the compiler
and native engine. Timestamp assistance inspects existing typed columns before
falling back to raw extraction; `timestamp_utc` names the output, not the input.
