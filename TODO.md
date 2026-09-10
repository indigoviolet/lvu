# lvu work plan

Current published version: **v0.1.5**. See [CHANGELOG.md](CHANGELOG.md) for what
each version shipped, and the [work ledger](docs/work-ledger.md) for validation
evidence. Completed checklist history is archived in
[docs/todo-history.md](docs/todo-history.md).

**Working** = actively being implemented or validated. **Ready** = accepted and
integrated, awaiting the next release. **Open** = unresolved, not currently assigned.
**Paused** = preserved unfinished work, deferred while feature delivery takes priority.

## Ongoing work

Parallel implementation and validation assignments; the supervisor owns review, integration and releases.

| Status | Work |
| --- | --- |
| **Working** | Compose live union views from existing views across sources, merged in selected timestamp order, with ordinary downstream filters and grouping. Preserve original record identities and capture ownership; define overlap, missing-time and dependency/revision behavior. Implementation integrated; final UI and release acceptance pending. |
| **Paused** | Fix memory autosave-flush shutdown failures on slow storage, preserving save acknowledgement and durability. Long-running validation is paused; the full volume-backed test remains unaccepted. |
| **Paused** | Diagnose and fix long blank viewports on 512 MB captures; keep useful loading/indexing progress visible while delivering the requested rows. Existing fixes and evidence are preserved; long-running validation is paused. |
| **Paused** | Measure and reduce cold-query journal contention without delaying capture indefinitely. Investigate the preserved 12.921-second cold query and writer/page-read scheduling; no causal explanation or full performance acceptance is claimed. |

## Active parallel assignments

These remaining items now have workers. Completion still requires executable
integration and the relevant acceptance evidence.

| Status | Work |
| --- | --- |
| **Working** | Make colour classification consume enrichment outputs rather than define independent pattern predicates. Preserve raw-text search as an explicit convenience exception. Ordinary-view implementation integrated; union colour threading and combined release acceptance remain pending. |
| **Working** | Replace raw-only special correlation extraction with enrichment-derived shared keys and ordinary filtering over union views. Preserve existing saved behavior. Replacement integrated; final UI and release acceptance pending. |
| **Working** | Let multiple lvu windows automatically share a background capture worker; independent views, detach on close, stop after the last window. |
| **Working** | Validate installation/terminal/process behavior on macOS and Windows (audit in docs/portability.md, checklist in docs/mac-test-plan.md). Automated arm64 macOS install-tree, kernel-PTY and process-cleanup validation is integrated; Intel macOS compile checks passed. Human terminal-emulator checks remain open, and Windows remains unsupported. arm64 Linux archive is built and CI-executed but has had no human interactive acceptance. |
