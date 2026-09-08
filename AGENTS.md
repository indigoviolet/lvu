# Working on lvu

Read `docs/architecture.md`, `TODO.md`, `docs/contracts.md`, and the current entries
in `docs/work-ledger.md` before editing. Use `docs/implementation-plan.md` for the
wider roadmap, not as evidence that a planned feature is implemented. The primary agent owns integration and this
ledger. Implementers work in assigned worktrees and only edit owned paths.

## Product invariants

- Preserve captured original bytes and stable identities. Parsing, enrichment,
  filtering, presentation, and cleanup must not silently destroy them.
- A record is captured once. A restarted source resumes from its durable cursor
  and repeats nothing. A record count can still exceed a line count, because a
  partial line is captured as its own record before its terminator arrives;
  that excess is what is in flight, never a function of the file's size. Do not
  assert an exact record count over a fixture whose lines are still arriving.
- Display raw data before derived data is ready. An invalid editor draft must
  leave the last valid applied view usable. Pending is not a failed predicate.
- Rust owns normal acquisition, Polars execution, and terminal rendering. Python
  constructs native Polars expressions when definitions change. Paseo is local.
- The query engine computes; the app names and presents. Before writing a scan,
  aggregate, sort, index or predicate in `lvu` or `lvu-view`, check whether Polars
  already provides it through `lvu-query` and use that. Hand-rolled evaluation is
  allowed only where the engine cannot express an invariant (raw bytes, stable
  identities, live append, presentation-only folding) and must say so in a comment
  naming which invariant. Never a second evaluator for something the engine has.
- Keep memory, queues, caches, scans, subprocess work, and redraw frequency bounded.
- No built-in charts/analytics workspace, headless product, jq engine, ACP backend,
  remote data staging, or provider-specific authentication project.

## Coordination

- Use mise for pinned tools, environment and developer/test tasks. Root mise.toml
  is primary-owned; propose new tasks or tool changes rather than racing edits.
  Run commands through `mise exec` or `mise run`. Do not independently modify
  global tool settings. Keep Polars build debug info/incremental disabled and
  compilation concurrency bounded on this disk-constrained development host.

- Build artifacts live on the large volume, never the root disk. The root disk is
  small and has hit 100% repeatedly; `/mnt/HC_Volume_106796581/lvu-build` has ample space.
  `mise.toml` points `CARGO_TARGET_DIR` at `/mnt/HC_Volume_106796581/lvu-build/target`
  for the primary checkout. An agent working in its OWN worktree must use its own
  target on that volume, because sharing one target directory between different
  worktrees makes cargo invalidate and rebuild the other's artifacts:
  `export CARGO_TARGET_DIR=/mnt/HC_Volume_106796581/lvu-build/$(basename "$PWD")-target`.
  Never put a target directory under /tmp or inside the worktree.
- Verify with `mise run test:pty:matrix`, which runs every PTY suite concurrently
  in about a minute. Rebase onto main and measure your baseline there before
  claiming a suite was already failing; other agents land fixes underneath you.
- Cargo never garbage-collects superseded artifacts, and a stale lvu-app test
  binary is ~386MB. Run `mise run janitor` to reclaim stale artifacts and
  abandoned PTY scratch (it never touches previews, captures or proof archives),
  and `mise run disk:check` before a long build.

- Do not edit another assignment's paths or shared manifests without contacting
  the primary agent. Propose interface changes in your completion report.
- Do not spawn further agents unless your assignment explicitly delegates that.
- Reverting a commit does not revert its data migration. Before backing out a
  change that bumps a workspace schema, set aside the migrated database it
  touched (`.lvu-captures/workspace/workspace.sqlite3`) or the older code will
  refuse to open it and every test inheriting that root fails for no code reason.
  Tests must never inherit the checkout's own capture root; the harness now
  injects a private one when neither `--capture-dir` nor `cwd` is given.
- Never merge into main, rewrite main, or touch another worktree. One coherent
  commit per bounded deliverable is preferred. Amend/fixup your own unpublished
  implementation revisions instead of accumulating debugging commits.
- Commit messages explain the problem/behavior and meaningful validation. Report
  commit IDs, tests actually run, failures, assumptions, and outstanding risks.
- Do not claim a milestone based on mocks when its exit gate requires integration.

## Validation

- Test observable behavior and product invariants, including malformed input,
  restart, stale work, limits, and subprocess failures where relevant.
- Rust formatting, targeted tests, then clippy for integrated Rust changes;
  typecheck and test TypeScript; verify Python/Rust expression outputs against
  the same fixtures. Do not create implementation-mirroring trivial tests.
- UI work requires Ratatui TestBackend checks and actual PTY interaction tests.
- No routine user QA or permission requests for authorized implementation work.
  One local working Paseo provider is sufficient for the shared integration.
- Keep test fixtures deterministic and do not read arbitrary user log data for
  benchmarks. Record skipped environment-dependent checks honestly.

## Extending current behavior

- Trace the executable wiring before declaring a feature complete. An isolated
  crate API, passing fixture, or returned topic commit is not app integration.
- Keep editor drafts, accepted constraints and candidate results separate. Rejecting
  a candidate must preserve the entire last-good chain, membership and live refresh.
  Never let progress from unpublished work advance accepted checkpoints.
- Preserve stable source/record/stage IDs across edits and persistence. Sources own
  capture; views share it. Restore must not silently launch remembered commands.
- Extend Polars support at the expression boundary, not by adding a second evaluator.
  Verify Python serialization through actual Rust execution. Shape and ID-order
  checks alone cannot prove value alignment or independence from batch boundaries.
  Retain bounds and reject unproven cross-record semantics with actionable errors.
- Inspect actual typed snapshot columns before extracting structured values from
  raw text. `timestamp_utc` is an output contract, never an assumed input field.
  Schema-valid model output still needs revision, dependency and data validation.
- UI geometry is shared by rendering, scrolling, mouse hitboxes and selection.
  Keep modal selection inside its visible surface, editable cursors visible and
  footers separate. Test narrow terminals and wide/combining Unicode characters.
- Use semantic theme roles on every surface. Product labels use 🧠 with an Agent
  ASCII fallback; keep Paseo and native/compiler implementation jargon out of
  ordinary user controls unless a diagnostic genuinely needs it.
- Child stdout/stderr must use owned pipes during the TUI. Restore terminal modes
  on startup failure, normal exit and panic. Test actual PTY behavior for changes
  involving redraw, selection, input or lifecycle.
- Do not confuse managed payload limits with RSS, derived-index caps with durable
  storage quotas, snapshot export limits with model inspection coverage, or a
  clipboard request with confirmed delivery.

## Documentation and previews

- Keep README focused on supported user behavior; architecture maps current code;
  TODO tracks unresolved work; the work ledger records validation evidence.
- Distinguish working-tree, integrated and published behavior. Update documentation
  when an implementation or test changes that status. Preserve reported failures
  until evidence resolves them; a focused rerun alone does not explain a race.
- Published previews are immutable. Build and test a new copied binary before
  moving `previews/latest`; record source revision, checksum and acceptance results.
- Preserve capture data, prior previews and proof archives. Never run a broad
  cleanup to recover build space; use targeted build-tool cleanup when necessary.
