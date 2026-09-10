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
| **Working** | Automatic shared capture: combined candidate `6c806ed` passes owner-reported same-cache two-window and sequential shutdown/reopen acceptance. The second-window blank-view blocker was an exclusive derived-index lock; window-specific overflow indexes fix contention without duplicating capture. Final candidate `873b3049` is independently cleared and merged into primary; the combined primary gate and release restore fix remain pending. |
| **Working** | Shared-worker admission dependency `4a7cd29` is independently source- and evidence-clear: 111 library and five real-worker tests pass. Combined controller includes it and is merged into primary; full primary validation remains pending. Superseded candidate failures remain in the ledger and retained logs. |
| **Working** | Shared-memory failure tracking: candidate `31b6158` preserves consumed save/recipe failures through Flush and clears them only on appropriate recovery. Independent Sol source review is clear; 27 memory tests passed, including three new test functions. Its gate exited 101 on controller-file lints. Applying the exact delta and validating the combined controller are authorized; source clearance is not integration acceptance. This is distinct from the paused slow-volume investigation. |
| **Working** | Remote-union commit: checked-in module discovery and append observation are present, but the test publication hook has a selection/replacement token race awaiting amendment. Window repair `a6ff2ea` includes committed shared prerequisites, an absolute receipt deadline, last-good retention and digest-field hardening; targeted tests pass and independent Sol review narrowly clears the three window files. Real transport status recovery, worker protocol integration and app acceptance remain pending. |
| **Working** | Two-window shared-capture acceptance: owner reports the full `9461790` specification passing on candidate `6c806ed` with the same cache, one worker, independent views, stable IDs, append after one window closes and clean final shutdown. Sequential reopen also passes. Sol verified the final evidence and cleared `873b3049`, now merged into primary. Full primary validation remains pending. |
| **Open** | Apple-silicon macOS human terminal acceptance: run the v0.1.6 checklist in Terminal.app and iTerm2. Automated arm64 installed-resource, kernel-PTY and process-cleanup evidence exists; human acceptance remains outstanding. Linux arm64 also lacks human interactive acceptance. Intel Macs and Windows are unsupported and are not acceptance targets. |

## Ready for the next release

| Status | Work |
| --- | --- |
| **Ready** | CLI help points to `/` → `Alt-A` for Advanced filtering (`4ccb1a9`). The v0.1.6 macOS checklist refresh is integrated (`b287894`); the checklist itself has not been executed. |
| **Ready** | Release/validation workflows and default formula renderer target Linux x86_64/arm64 and Apple-silicon macOS only. Intel formula rendering is available solely through an explicit historical option. Six local fixture checks pass; no hosted run or new release is claimed. |
