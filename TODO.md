# lvu work plan

Current published version: **v0.1.2**. See [CHANGELOG.md](CHANGELOG.md) for what
each version shipped, and the [work ledger](docs/work-ledger.md) for validation
evidence. Completed checklist history is archived in
[docs/todo-history.md](docs/todo-history.md).

**Working** = actively being implemented or validated. **Ready** = accepted and
integrated, awaiting the next release. **Open** = unresolved, not currently assigned.

## Ongoing work

Parallel implementation and validation assignments; the supervisor owns review, integration and releases.

| Status | Work |
| --- | --- |
| **Working** | Unify Grouping with Run and Filter modes over enrichment outputs: consecutive equal keys, or non-null event starts with all intervening records collapsed. Move pattern definition out of grouping, preserve exact key equality and legacy saved meaning. v0.1.3 publication is held for this correction. |
| **Working** | Let source assistance propose and add multiple reviewed sources in one request, with bounded results, stable identities and no execution before approval in the application. |
| **Working** | Fix memory autosave-flush shutdown failures on slow storage, preserving save acknowledgement and durability. The full volume-backed soak remains unaccepted. |
| **Working** | Diagnose and fix long blank viewports on 512 MB captures; keep useful loading/indexing progress visible while delivering the requested rows. |
| **Working** | Investigate enrichment-chain/filter mismatches and replace missing-column implementation errors with actionable diagnostics, preserving the last valid view. |
| **Working** | Measure and reduce cold-query journal contention without delaying capture indefinitely. Investigate the preserved 12.921-second cold query and writer/page-read scheduling; no performance or full-soak pass is claimed yet. |

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
| **Open** | Align other derived-data consumers with enrichment: built-in timestamp recognition/severity mapping, direct colour predicates and raw-only correlation keys remain separate paths. Basic structured input decoding and raw search are deliberate boundaries to settle. |
| **Open** | Display time zones are fixed UTC offsets only: no timezone database, so daylight saving is never applied and a named zone (`Europe/Berlin`) cannot be chosen. Every displayed timestamp carries its offset; the Settings help line says so. |
| **Open** | Let multiple lvu windows automatically share a background capture worker; independent views, detach on close, stop after the last window. |
| **Open** | Validate installation/terminal/process behavior on macOS and Windows (audit in docs/portability.md, checklist in docs/mac-test-plan.md; no run yet). arm64 Linux archive is built and CI-executed but has had no human interactive acceptance. |
