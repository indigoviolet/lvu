# Working on lvu

Read `docs/implementation-plan.md`, `docs/contracts.md`, and your assignment in
`docs/work-ledger.md` before editing. The primary agent owns integration and this
ledger. Implementers work in assigned worktrees and only edit owned paths.

## Product invariants

- Preserve captured original bytes and stable identities. Parsing, enrichment,
  filtering, presentation, and cleanup must not silently destroy them.
- Display raw data before derived data is ready. An invalid editor draft must
  leave the last valid applied view usable. Pending is not a failed predicate.
- Rust owns normal acquisition, Polars execution, and terminal rendering. Python
  constructs native Polars expressions when definitions change. Paseo is local.
- Keep memory, queues, caches, scans, subprocess work, and redraw frequency bounded.
- No built-in charts/analytics workspace, headless product, jq engine, ACP backend,
  remote data staging, or provider-specific authentication project.

## Coordination

- Do not edit another assignment's paths or shared manifests without contacting
  the primary agent. Propose interface changes in your completion report.
- Do not spawn further agents unless your assignment explicitly delegates that.
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
