# lvu work plan

Current published version: **v0.1.6**. See [CHANGELOG.md](CHANGELOG.md) for what
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

## Active parallel assignments

Supervisor handover refreshed 2026-09-10. These are implementation assignments,
not claims of integrated or published behavior. Completed cleanup reports 69.8 GiB free; bounded validation can resume under
the shared lock after checking the 6 GiB free-space gate.

| Status | Work |
| --- | --- |
| **Open** | Prevent build-output accumulation: implement explicit retirement for completed worktree/release targets while preserving named investigation binaries and proof. Cleanup is complete: lvu-build fell from 85.7 to 19.8 GiB, with 69.8 GiB volume free and a running 3 GiB compiler cache. Ordinary janitor omits release/integration names and retires existing worktrees only by `main` containment; current integration uses a separate branch. |
| **Working** | Automatic shared capture: final controller `60648d2` and restore fix `1e32fdb` are independently Terra-cleared and integrated through primary `5fddf2e`. Same-cache windows share one worker/journal, per-window derived indexes avoid writer-lock contention, session writes have one worker owner, and sequential reopen preserves exact records with no replay. Focused combined tests and app Clippy pass; the responsive-dialog work and complete release gate remain pending. |
| **Working** | Shared-worker admission and failure tracking are integrated in the final controller ancestry. Schema/path validation precedes deduplication; failed save/recipe outcomes remain authoritative through Flush. Their former standalone holds are resolved, but complete workspace/PTY acceptance still belongs to the final candidate. |
| **Working** | Remote-union commit is integrated through `60648d2`: sorted progress guards pin attestation through settlement, actual framed worker commit/status/foreign paths pass, and the application preserves original identity across lost/delayed replies under one absolute deadline without cancelling a client exchange mid-cleanup. Final release proof must rerun every stage on the combined binary. |
| **Working** | Two-window acceptance now includes checked-in draining sequential close/append/reopen coverage (`77018fc`) with settled exact bytes, journal-prefix extension, socket/worker cleanup and no replay. It is integrated and focused-green; the full primary PTY matrix has not yet run on the dialog-complete candidate. |
| **Working** | Responsive dialogs: the exhaustive audit and reviewed View segmented-header baseline are integrated. Phase-A shared core `b892d19` is held on action reachability, row bounds, frozen-anchor placement, popup gaps and render/geometry agreement; a successor is active. No component migration starts before the corrected core is Terra-cleared. |
| **Open** | Apple-silicon macOS human terminal acceptance: run the v0.1.6 checklist in Terminal.app and iTerm2. Automated arm64 installed-resource, kernel-PTY and process-cleanup evidence exists; human acceptance remains outstanding. Linux arm64 also lacks human interactive acceptance. Intel Macs and Windows are unsupported and are not acceptance targets. |

## Ready for the next release

| Status | Work |
| --- | --- |
| **Ready** | CLI help points to `/` → `Alt-A` for Advanced filtering (`4ccb1a9`). The v0.1.6 macOS checklist refresh is integrated (`b287894`); the checklist itself has not been executed. |
| **Ready** | Release/validation workflows and default formula renderer target Linux x86_64/arm64 and Apple-silicon macOS only. Intel formula rendering is available solely through an explicit historical option. Six local fixture checks pass; no hosted run or new release is claimed. |
| **Ready** | View operations New blank, Clone, Rename and Sources are modes in one segmented header; Apply / Apply membership is the sole action and filled default. Keyboard, mouse, narrow-layout and actual named-view PTY coverage are integrated. |
