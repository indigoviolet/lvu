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
not claims of integrated or published behavior. The first cleanup restored 8.2 GiB free; bounded validation can resume under
the shared lock after checking the 6 GiB free-space gate.

| Status | Work |
| --- | --- |
| **Working** | Restore build headroom and define target retirement. Audit measured 85.7 GiB: 38.2 GiB worktree targets, 27.8 GiB release/integration targets, 3.4 GiB primary target, 10.0 GiB compiler cache, 6.3 GiB scratch/proof/other. Release owner is inventorying superseded gate targets; preserve captures, proof and required investigation binaries. Ordinary janitor misses release/integration target names and only retires existing worktrees whose branches are in `main`, whereas current integration is on a separate branch. The supervisor reclaimed 4.2 GiB from the superseded a97 gate, preserving accepted binaries and evidence; broader retirement remains in progress. |
| **Working** | Automatic shared capture across ordinary windows: acquisition/controller cutover, feeder cancellation, stop/detach and last-window shutdown. Owner's cutover remains uncommitted and unvalidated; controller session was closed at handover and needs continuation. Published v0.1.6 does not include this. |
| **Working** | Shared-worker admission: serialize concurrent fresh-ID starts, resume stopped matching sources, and deduplicate canonical file aliases without changing acquisition policies. Separate worker-side implementation; independent review and real-connection tests required. |
| **Working** | Shared-memory failure tracking: candidate `0614340` preserves consumed save/recipe failures through Flush and clears them only on appropriate recovery. Independent Sol review assigned; three new real-stack regression tests and clippy are now authorized after cleanup. This is distinct from the paused slow-volume investigation. |
| **Working** | Remote-union commit: fixed 32-byte digest and bounded window routing have narrow API clearance. Publication hook `3bf278e` needs its observation bound to the appended four-record state and ordinary checked-in test discovery. Window-side commitment, receipt fencing and worker protocol integration continue separately; none is app acceptance. |
| **Working** | Two-window shared-capture acceptance: test checkpoint `9461790` has syntax/whitespace checks only. Finish conflict/reload coverage against the concrete reopen/reconcile interface, then prove shared capture, independent views, detach and final shutdown in actual PTYs. |
| **Open** | Apple-silicon macOS human terminal acceptance: run the v0.1.6 checklist in Terminal.app and iTerm2. Automated arm64 installed-resource, kernel-PTY and process-cleanup evidence exists; human acceptance remains outstanding. Linux arm64 also lacks human interactive acceptance. Intel Macs and Windows are unsupported and are not acceptance targets. |
| **Open** | Align future release/validation matrices and packaging with the supported platforms: Linux x86_64/arm64 and Apple-silicon macOS. Existing automation still includes Intel Darwin and Windows diagnostic checks. Preserve historical release archives and evidence; do not treat them as a support commitment. |

## Ready for the next release

| Status | Work |
| --- | --- |
| **Ready** | CLI help points to `/` → `Alt-A` for Advanced filtering (`4ccb1a9`). The v0.1.6 macOS checklist refresh is integrated (`b287894`); the checklist itself has not been executed. |
