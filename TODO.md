# lvu work plan

Current published version: **v0.1.8**. See [CHANGELOG.md](CHANGELOG.md) for what
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
| **Paused** | Fix memory autosave-flush shutdown failures on slow storage, preserving save acknowledgement and durability. Long-running validation is paused; the full volume-backed test remains unaccepted. |
| **Paused** | Diagnose and fix long blank viewports on 512 MB captures; keep useful loading/indexing progress visible while delivering the requested rows. Existing fixes and evidence are preserved; long-running validation is paused. |
| **Paused** | Measure and reduce cold-query journal contention without delaying capture indefinitely. Investigate the preserved 12.921-second cold query and writer/page-read scheduling; no causal explanation or full performance acceptance is claimed. |

## Active work

Published v0.1.8 removed the shared-capture and responsive-dialog delivery rows
from this list. Remaining entries are unresolved follow-up work, not release
holds.

| Status | Work |
| --- | --- |
| **Open** | Prevent build-output accumulation: implement explicit retirement for completed worktree/release targets while preserving named investigation binaries and proof. Cleanup is complete: lvu-build fell from 85.7 to 19.8 GiB, with 69.8 GiB volume free and a running 3 GiB compiler cache. Ordinary janitor omits release/integration names and retires existing worktrees only by `main` containment; current integration uses a separate branch. |
| **Open** | Optionally retain and display a bookmark's originating view. Bookmarks are deliberately source-scoped today, so the durable record identity and note survive across views but no origin-view metadata is stored; adding it needs a separate compatible schema/product decision. |
| **Open** | Apple-silicon macOS human terminal acceptance: run the v0.1.8 checklist in Terminal.app and iTerm2. The v0.1.8 archive built and executed on native CI; human acceptance remains outstanding. Linux arm64 also lacks human interactive acceptance. Intel Macs and Windows are unsupported and are not acceptance targets. |

## Ready for the next release

None. The accepted shared-capture, responsive-dialog, View-mode and release
workflow work shipped in v0.1.8.
