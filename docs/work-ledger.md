# Implementation work ledger

## Verified baseline

- Repository initially empty on unborn main, no pre-existing user changes.
- User authorized full plan, Paseo implementer subagents, independent testing,
  primary review/integration, isolated worktrees and coherent local commits.
- Configured profile: GPT-5.6-Sol-Implementer → codex/gpt-5.6-sol, full-access.
- Rust was not found on the initial shell PATH; toolchain setup is primary-owned.
- No implementation milestone is complete yet.

## Initial ownership

| Package | Owner | Owned paths | Gate |
|---|---|---|---|
| Plan/contracts/integration | Primary | docs/, AGENTS.md, README.md, root orchestration | Review all integration |
| M1A capture foundation | Paseo core implementer | Cargo.toml, Cargo.lock, crates/lvu-core/, .gitignore | Lossless recovery + file/command fixture tests |
| M1B Polars compatibility | Paseo expression implementer | spikes/polars-interop/, python/ | Real differential Python/Rust execution |
| M1C Paseo bridge | Paseo bridge implementer | bridge/ | Protocol tests + one local provider proof |

Each assignment must return actual test evidence and a coherent topic commit.
Primary alone marks gates verified and integrates. Later chunks are assigned only
after dependencies and shared interfaces have been reviewed.

## Active handoffs

| Package | Agent ID | Workspace / branch |
|---|---|---|
| M1A | 0362e773-1202-4d72-9b18-759630393427 | wks_56c5eff0a16e126c / impl/capture-foundation |
| M1B | cafce070-4df7-4d87-a010-2cbf64c7afb0 | wks_318ae218cc65c7d7 / impl/polars-interop |
| M1C | e66a7d58-6cfb-4388-9cad-fce66cca2308 | wks_6461b42f06c887ad / impl/paseo-bridge |

All are attached Paseo subagents using the requested profile, with completion
notifications enabled. Each received full context, path ownership, test gates,
mise requirements, resource constraints, and commit/review instructions.

User added mise as the environment/task/tool manager. Primary owns mise.toml,
pins exact versions and installs Rust with a minimal profile plus fmt/clippy.
Initial disk availability was 5.4 GiB; avoid duplicate heavy Polars build artifacts.

Environment validation passed: `mise install`, `mise run doctor`,
`mise tasks validate`, and `git diff --check`. Verified Rust/Cargo 1.98.1,
Python 3.12.14, Node 26.8.1, uv 0.12.10. After installation free disk was 3.8 GiB.
These are environment checks, not implementation acceptance results.

## M1A review round 1

Submitted commit: `1d75265641230b9e60c786c81dc0016a7bb4ac5a` (not integrated).
Primary independently reran `mise run check:rust`: six tests, fmt and clippy pass.
Independent regression crate `/tmp/lvu-review-m1a` found four failures:

- File cancellation hangs when the event queue is full.
- Command cancellation is ignored after stdout/stderr close but child stays alive.
- A corrupted frame length is accepted as a torn tail and recovery deletes bytes.
- Appending to a relative journal path fails when syncing its empty parent path.

Code review additionally requires bounded recovery/page reads, reliable subprocess
reader/descendant cleanup, sequence reservations without per-record fsync, poisoned
writer handling after partial I/O, enforced single-writer ownership, noncolliding
sidecar paths, and readable/event-driven file-follow code and tests. Unsupported
restart policies must fail explicitly until implemented.

Returned all findings to the same owner with reproduction tests and amendment
instructions. M1A acceptance remains pending. No submitted implementation was
merged merely because its original test suite passed.

## M1B review round 1

Submitted commit: `863ff74` (not integrated). Primary reran Python tests (11),
Rust tests (2), clippy and the 24-cell differential test; all original checks pass.
The Python-to-Rust expression serialization/execution mechanism is verified.

Independent probes found that the helper accepts non-row-local forward-fill,
the Rust serialized-expression denylist accepts a tampered Reverse function and
reorders rows, and malformed JSON field types raise uncaught Python exceptions.
The Rust protocol reader also allocates an unbounded line before its payload
limit. Requested fail-closed operation validation, bounded envelope reads and
structured protocol errors with regression tests. Raw/identity read restrictions
must be removed (write protection remains), and ordinary regex contains added.

Returned corrections to the same owner, who will amend the unpublished commit.
M1B's mechanism proof succeeds; its reusable validation gate remains pending.

## M1C review round 1

Submitted commit: `cb088b8` (not integrated). Primary reran typecheck, 15 tests and
production build successfully. Independent probes reproduced concurrent session
creation exceeding maxSessions=1 and stream events continuing after cancellation.
Review requires capacity reservation, cancellation of pending observations,
remote-versus-observed turn state, bounded input/output/in-flight work, closed-state
handling, and concrete proposal schemas consistent with shared source definitions.

The original live check used models different from the known-working implementer
profile. Requested one proof with codex/gpt-5.6-sol, full-access, and verification
of supported local Paseo cancellation APIs/CLI. No user login or provider matrix
requested. Owner will amend the unpublished implementation after regression tests.

## M1B review round 2

Candidate `2e21cd2`: primary reran 16 Python tests, 3 Rust tests, clippy and 56
whole/partitioned differential values; all passed. Typed Rust allowlisting,
bounded envelope reads and earlier protocol/cancellation-independent fixes verified.
Two narrow corrections remain: Python audit treats literal text such as "Reverse"
as an operation, and date/time parsing still allows batch-dependent format inference.
Requested structure-aware auditing and explicit parsing formats with regressions.
No expanded scope; integration follows these final semantic checks.

## M1D independent UI work started

Assigned Ratatui state/render/terminal shell and real Python PTY tests to a new
GPT-5.6-Sol-Implementer subagent in workspace `wks_eb8df086de278f28`, branch
`impl/tui-shell`. It owns only `crates/lvu/**` and `tests/pty/**`, using a temporary
standalone manifest until primary integrates the reviewed workspace. It does not
implement competing acquisition/query logic; fixture-backed UI is explicitly
labeled until production adapters are wired. Acceptance includes TestBackend and
actual PTY keyboard/mouse/paste/resize/follow/restore checks. No user QA needed.

## M1A review round 2

Candidate `eca13fd`: primary reran 15 in-tree tests, fmt/clippy, and all four
original independent regressions; passed. Review of new paging API found a new
error path: failed page read leaves the shared file cursor inside history, so the
next append overwrites captured bytes. Added an independent fifth regression,
reproduced failure, and returned the narrow append-position fix to the owner.
Also requested explicit page-budget overshoot and abort-versus-graceful-stop
semantics in documentation. Integration remains gated on preserving original data.
