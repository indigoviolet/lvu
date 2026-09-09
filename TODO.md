# lvu work plan

Current published version: **v0.1.3**. See [CHANGELOG.md](CHANGELOG.md) for what
each version shipped, and the [work ledger](docs/work-ledger.md) for validation
evidence. Completed checklist history is archived in
[docs/todo-history.md](docs/todo-history.md).

**Working** = actively being implemented or validated. **Ready** = accepted and
integrated, awaiting the next release. **Open** = unresolved, not currently assigned.

## Ongoing work

Parallel implementation and validation assignments; the supervisor owns review, integration and releases.

| Status | Work |
| --- | --- |
| **Working** | Compose live union views from existing views across sources, merged in selected timestamp order, with ordinary downstream filters and grouping. Preserve original record identities and capture ownership; define overlap, missing-time and dependency/revision behavior. |
| **Working** | Fix memory autosave-flush shutdown failures on slow storage, preserving save acknowledgement and durability. The full volume-backed soak remains unaccepted. |
| **Working** | Diagnose and fix long blank viewports on 512 MB captures; keep useful loading/indexing progress visible while delivering the requested rows. |
| **Working** | Measure and reduce cold-query journal contention without delaying capture indefinitely. Investigate the preserved 12.921-second cold query and writer/page-read scheduling; no performance or full-soak pass is claimed yet. |

## Accepted for the next release

Integrated after v0.1.3; outside its immutable tag and installed runtime.

| Status | Work |
| --- | --- |
| **Ready** | Make “give me a UTC timestamp column” and “give me a severity column” assistance shortcuts that propose ordinary enrichment definitions for review; let each view select the output columns for time/severity roles. Replace the separate automatic recognition/normalization paths. Capture time remains acquisition metadata. |

## Active parallel assignments

These remaining items now have workers. Completion still requires executable
integration and the relevant acceptance evidence.

| Status | Work |
| --- | --- |
| **Working** | Make colour classification consume enrichment outputs rather than define independent pattern predicates. Preserve raw-text search as an explicit convenience exception. |
| **Working** | Replace raw-only special correlation extraction with enrichment-derived shared keys and ordinary filtering over union views. Preserve existing saved behavior until its replacement is integrated. |
| **Working** | Display time zones are fixed UTC offsets only: no timezone database, so daylight saving is never applied and a named zone (`Europe/Berlin`) cannot be chosen. Every displayed timestamp carries its offset; the Settings help line says so. |
| **Working** | Let multiple lvu windows automatically share a background capture worker; independent views, detach on close, stop after the last window. |
| **Working** | Validate installation/terminal/process behavior on macOS and Windows (audit in docs/portability.md, checklist in docs/mac-test-plan.md; no run yet). arm64 Linux archive is built and CI-executed but has had no human interactive acceptance. |
