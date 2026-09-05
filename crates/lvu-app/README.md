# lvu-app composition

`lvu-app` is the temporary real-source executable. It composes the accepted
capture runtime with the live journal row provider and the Ratatui UI without
duplicating their acquisition, storage, or paging logic.

```text
lvu-app --capture-dir ./captures --file ./service.log
lvu-app --capture-dir ./captures --command 'make serve'
```

`--file` and `--command` are repeatable. Commands are explicitly executed as
`sh -c` with the application's current directory recorded in the source
definition. With no source arguments, `lvu-app` opens a bounded Add source
dialog. Tab completes file paths without invoking a shell; ambiguous matches are
listed and can be selected with the arrow keys and Tab. Alt-F selects file input
and Alt-C selects command input. Completion expands `~/` for enumeration while
preserving the typed tilde form, and command completion is intentionally disabled.

Press Ctrl-D in the source dialog to run a bounded, asynchronous discovery scan.
The scan combines Docker, Linux `/proc`, and the current project providers;
recent-source input is empty until persisted memory is composed. Type to filter
the returned candidate list, use the arrow keys to select, and press Enter to
explicitly start it. Discovery never starts a candidate autonomously. Ctrl-R
cancels any active generation and starts a fresh scan. Provider failures,
partial limits, timeouts, cancellation, and an empty result remain visible.

## Integration boundary

The UI carries an opaque candidate fingerprint while `lvu-app` retains the full
`DiscoveryCandidate`. Selection passes its authoritative `SourceDefinition`
directly to `SourceManager`; it does not reconstruct paths or Docker commands.
Normal source admission, duplicate reuse, registration rollback, and shutdown
remain shared with manual sources. A future memory composition can populate
`ProjectConfig::recent_sources` without changing the UI contract.

The application registers the same source handles and raw views with
`NativeViewAdapter`. Literal searches execute entirely in Rust and incrementally
refresh as capture advances. Advanced Polars text is compiled lazily through the
locked `python/` project using `mise exec -- uv run`; the helper is never started
for literal-only searches. Both constraints are applied together with AND, while
invalid advanced drafts leave the last accepted view and its live refresh active.
The terminal uses the adapter's cloneable row handle and passes its mutable query
half through the composition tick, avoiding duplicate mutable ownership.

Press `e` to edit one named native enrichment as
`field_name = Python Polars expression`. Applying it uses the same locked compiler
and native batch engine as advanced filtering; the derived scalar is available to
details, pins, color-by-value, and advanced filters for retained rows and new
arrivals. The editor shows a representative raw before/applied-after value.
Invalid candidates preserve the last accepted enrichment and membership, empty
input clears it, and original raw text and stable IDs never become writable.
This slice intentionally supports one active named enrichment per view; command
enrichments, AI authoring, nested expansion, and stacktrace grouping remain out
of scope.

Press `d` to show the selected event's original raw text and recognized scalar
JSON/logfmt fields. Press `i` for the field picker, use arrows or the mouse to
select a field, Space/Enter to pin or unpin it as a column, and `c` to toggle
stable color-by-value. Pins and the color field are stored with the working view.
Projection is limited to visible live-adapter rows; nested values are not expanded
and runtime fragments remain physical records.

The root workspace includes `lvu-view` and its locked dependencies.

Press `v` on a selected source view to manage independent named views. Alt-B
creates a blank view, Alt-D clones accepted settings plus editor drafts and
presentation, and Alt-R renames the selected view. All views reuse the source's
single capture handle and retain independent filters, enrichment, navigation,
pins, and colors. The application admits at most 16 views per source and 128
views globally before registering query state. This slice does not create merged
multi-source views or expose recipe import/export.
