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
not claims of integrated or published behavior. Cleanup agents report about 63 GiB free; bounded validation can resume under
the shared lock after checking the 6 GiB free-space gate.

| Status | Work |
| --- | --- |
| **Working** | Restore build headroom and define target retirement. Audit measured 85.7 GiB: 38.2 GiB worktree targets, 27.8 GiB release/integration targets, 3.4 GiB primary target, 10.0 GiB compiler cache, 6.3 GiB scratch/proof/other. A dedicated disk agent owns remaining worktree inventory and guarded cleanup; preserve captures, proof and required investigation binaries. Ordinary janitor misses release/integration target names and only retires existing worktrees whose branches are in `main`, whereas current integration is on a separate branch. The latest cleanup report puts lvu-build at 26.9 GiB, down from 85.7 GiB, with required binaries and evidence preserved. The repo and running server cache cap are now 3 GiB; coordinated cache maintenance is complete. The disk agent owns the remaining inventory and disposal; target-retirement policy remains unfinished. |
| **Working** | Automatic shared capture across ordinary windows: acquisition/controller cutover, feeder cancellation, stop/detach and last-window shutdown. Controller checkpoint `fb7304b` has targeted test evidence but remains unaccepted: acquisition is still local, shutdown can lose Stop or discard detach errors, and the thread is not joined. Repairs and acquisition routing are assigned. Published v0.1.6 does not include this. |
| **Working** | Shared-worker admission: serialize concurrent fresh-ID starts, resume stopped matching sources, and deduplicate canonical file aliases without changing acquisition policies. Separate worker-side implementation; independent review and real-connection tests required. |
| **Working** | Shared-memory failure tracking: candidate `31b6158` preserves consumed save/recipe failures through Flush and clears them only on appropriate recovery. Independent Sol source review is clear; 27 memory tests passed, including three new test functions. Its gate exited 101 on controller-file lints. Applying the exact delta and validating the combined controller are authorized; source clearance is not integration acceptance. This is distinct from the paused slow-volume investigation. |
| **Working** | Remote-union commit: fixed 32-byte digest and bounded window routing have narrow API clearance. Publication hook `3bf278e` needs its observation bound to the appended four-record state and ordinary checked-in test discovery. Window candidate `ae31b72` has targeted in-process evidence but lacks committed shared API dependencies and a total receipt-wait deadline. Independent review requires bounded wait/recovery and digest-contract hardening before combined validation; worker protocol integration and app acceptance remain pending. |
| **Working** | Two-window shared-capture acceptance: test checkpoint `9461790` has syntax/whitespace checks only. Finish conflict/reload coverage against the concrete reopen/reconcile interface, then prove shared capture, independent views, detach and final shutdown in actual PTYs. |
| **Open** | Apple-silicon macOS human terminal acceptance: run the v0.1.6 checklist in Terminal.app and iTerm2. Automated arm64 installed-resource, kernel-PTY and process-cleanup evidence exists; human acceptance remains outstanding. Linux arm64 also lacks human interactive acceptance. Intel Macs and Windows are unsupported and are not acceptance targets. |

## Ready for the next release

| Status | Work |
| --- | --- |
| **Ready** | CLI help points to `/` → `Alt-A` for Advanced filtering (`4ccb1a9`). The v0.1.6 macOS checklist refresh is integrated (`b287894`); the checklist itself has not been executed. |
| **Ready** | Release/validation workflows and default formula renderer target Linux x86_64/arm64 and Apple-silicon macOS only. Intel formula rendering is available solely through an explicit historical option. Six local fixture checks pass; no hosted run or new release is claimed. |
