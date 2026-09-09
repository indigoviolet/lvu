# lvu work plan

Current published version: **v0.1.2**. See [CHANGELOG.md](CHANGELOG.md) for what
each version shipped, and the [work ledger](docs/work-ledger.md) for validation
evidence. Completed checklist history is archived in
[docs/todo-history.md](docs/todo-history.md).

**Working** = actively being implemented or validated. **Ready** = accepted and
integrated, awaiting the next release. **Open** = unresolved, not currently assigned.

## Ongoing work

Actively being worked now.

| Status | Work |
| --- | --- |
| **Working** | Readable multiline output through reversible Grouping/Folding: wrapped messages, pretty-printed payloads, diagnostics, stack traces and stray continuation lines. Conservative bounded event recognition; every original byte and record preserved, incomplete output stays visible. Corrected implementation is awaiting final validation and integration; not released. |

## Accepted for the next release

Integrated after v0.1.2; outside the immutable v0.1.2 tag and installed runtime.

| Status | Work |
| --- | --- |
| **Ready** | Status prioritizes actionable notices, event-time diagnostics, row ranges and visible return/help controls; wide/combining text fits and optional indicators give way as whole segments. |
| **Ready** | FOLLOW draws the newest servable window while preserving true-tail selection. HISTORY keeps its requested destination separate from retained displayed rows, retries without another keypress, and reports the actual range. |
| **Ready** | Pending raw-context location is visible in narrow status lines: the protected prefix says `locating #N` until the chase resolves or reports its bounded failure. Full origin context remains optional when space fits. |

## Upcoming backlog

Unresolved and not currently assigned. No new product commitments beyond what is listed.

| Status | Work |
| --- | --- |
| **Open** | Diagnose the volume-backed soak failures: a 12.921 s first cold query and memory autosave-flush shutdown failures, with shared capture/journal `RecordBytes` already in v0.1.2. No full-soak pass or capture performance promise is claimed. |
| **Open** | Diagnose long blank viewports on 512 MB captures and show useful loading/indexing progress while rows are pending. Progress display is not a substitute for delivering the rows. |
| **Open** | A filter naming a column its own enrichment chain has not produced shows Polars' lowering error verbatim. Investigate why the chain and the filter compiled against it can disagree, and report the cause (`error_flag is not produced by this chain`) instead of the implementation. The engine's `dependency_unavailable` guard covers a stage that ran and failed, not one absent from the chain. Pinned by `a_filter_naming_a_column_no_stage_produces_reports_polars_lowering_verbatim` in `crates/lvu-query/tests/query.rs`. |
| **Open** | Display time zones are fixed UTC offsets only: no timezone database, so daylight saving is never applied and a named zone (`Europe/Berlin`) cannot be chosen. Every displayed timestamp carries its offset; the Settings help line says so. |
| **Open** | Let multiple lvu windows automatically share a background capture worker; independent views, detach on close, stop after the last window. |
| **Open** | The writer thread serves every page read between appends, so a query paging the journal waits behind capture's own appends. Measure before assuming it matters. |
| **Open** | `test_empty_event_fields_pty.py` loses its 3 s wait for `Fields closed` at load 15 or so, independent of any recent change (1 in 8 against both pre- and post-fix binaries). Needs a bound tied to something the app actually signals, not a wider number. |
| **Open** | Validate installation/terminal/process behavior on macOS and Windows (audit in docs/portability.md, checklist in docs/mac-test-plan.md; no run yet). arm64 Linux archive is built and CI-executed but has had no human interactive acceptance. |
