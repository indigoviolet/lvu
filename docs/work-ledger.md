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
Agent ID: `66889b2a-f524-4117-9ed0-492670dce8d6`.

## M1A review round 2

Candidate `eca13fd`: primary reran 15 in-tree tests, fmt/clippy, and all four
original independent regressions; passed. Review of new paging API found a new
error path: failed page read leaves the shared file cursor inside history, so the
next append overwrites captured bytes. Added an independent fifth regression,
reproduced failure, and returned the narrow append-position fix to the owner.
Also requested explicit page-budget overshoot and abort-versus-graceful-stop
semantics in documentation. Integration remains gated on preserving original data.

## M1B accepted

Reviewed and integrated owner commit `913ab7f` after primary inspection and rerun:
20 Python tests, 3 Rust tests, fmt/clippy, 56 whole/single-row-partitioned differential
cells and malformed/oversized/tampered-expression diagnostics passed. This accepts
the bounded compatibility proof and helper, not a completed production query engine.
Added root mise expression tasks. Production helper lifecycle, application schema,
batch enrichment/filter execution and generation fencing are the next query chunk.

## M1A accepted

Integrated owner commit `141bbab` as `5f93d33` after primary inspection, 17 in-tree
tests and all five independent regressions passed. Reran mise check:rust from main
using the existing build cache; fmt, tests and clippy passed. Append-mode writes
preserve captured history after paging errors. Accepted scope excludes HTTP and
restart execution and does not claim abort cancellation is graceful lossless stop.

## Next active work packages

| Package | Agent | Workspace/branch | Owned paths |
|---|---|---|---|
| Native batch query + compiler host | f458c358-f060-44ea-9cdd-884dfa1051c2 | wks_a61b266ef67bba8d / impl/query-engine | crates/lvu-query/, python/, spikes/polars-interop/ |
| Local source discovery | 48ae1965-0fcd-4ff8-ac21-b82740d27ebb | wks_a8da54ed1efcc919 / impl/source-discovery | crates/lvu-discovery/ |

Both use the requested implementer profile, full acceptance briefings and new
worktrees from reviewed main. Query ownership supersedes the completed compatibility
owner. Root Cargo and mise remain primary-owned. TUI shell and bridge corrections
continue in their existing isolated assignments. Complete M1 awaits integrated
live viewer, production query wiring, PTY gate and successful typed Paseo proposal.

## M1C review round 2

Candidate `044051c`: primary reran 30 tests, typecheck and build successfully.
Independently read archived live session `549d4f82-4dd7-4aae-8911-10e5462d95c7`:
actual fixture read and typed error-level filter with exact revisions verified.
No additional provider authentication testing needed.

Reproduced resolved SDK timeout incorrectly freeing remote-busy session. Requested
status-aware completion, resumed-session state updates, duplicate resume protection,
connection/create shutdown races and late-owned-session cleanup, bounded permanently
stalled stdout shutdown, and CLI cancellation bound to the configured SDK endpoint.
Returned targeted regressions to bridge owner; live proposal gate itself is passed.

## Capture runtime assigned

Reused accepted core implementer `0362e773-1202-4d72-9b18-759630393427` for
`impl/capture-runtime`, rebased onto reviewed main in its existing workspace.
Owns `crates/lvu-core/**` and new `crates/lvu-ingest/**` only. Assignment adds
durable source-manager wiring, bounded writer/page work, metadata/status boundaries,
graceful drain versus explicit abort, progressive partial-line capture, quotas,
and end-to-end acquisition/journal tests. Shared manifests remain primary-owned.

## M1D review round 1

Candidate `558e080`: primary reran six state tests, clippy and actual PTY workflow;
all pass. Actual initial screen inspection showed only final row with an otherwise
blank viewport: sync happened before layout height and ignored later size change.
Requested layout-aware positioning/header accounting, per-view drafts/applied state,
nonblocking generation-correlated query dispatch, bounded input/requests, empty
workspace behavior, focus/modal-aware hit testing and robust terminal screen parsing.
Existing tests missed visible first-screen and mouse-coordinate errors. Returned
concrete regression requirements and amendment instructions to UI owner.

## Discovery review round 1

Candidate `885d792`: seven tests and clippy passed, including actual owned tee and
pipe nonconsumption. Independent `/tmp/lvu-review-discovery` tests reproduced
replacement of persisted recent SourceId and Docker drain taking two seconds past
a 100 ms deadline when a descendant inherited stdout. Requested lifetime bounds,
preserved saved identity, limits inside enumeration/read loops, valid individual
container log commands, lossless OS-path identity and evidence merge at capacity.
Live Docker remains skipped due socket permission; no user intervention requested.

## Bridge review round 3

Candidate `88cb1de`: 38 tests, typecheck and build independently pass. Remote
status tracking and supported endpoint-bound CLI stop reviewed; live stop trace
confirmed. A narrow remaining capacity bug was reproduced: three timed-out creates
remain unresolved despite maxSessions=1 because reservations release on observation
timeout. Requested reservation ownership through SDK settlement/late cleanup and
regressions; other bridge scope frozen before integration.

## Production query review round 1

Candidate `cc3ca96`: all 11 integration tests pass independently. External
`/tmp/lvu-review-query` reproduced four failures: cancellation after final batch
still commits; null identity silently misaligns masks; JSON numeric status becomes
String and cannot be compared to 500; blocked helper stdin exceeds its deadline
(2.014 seconds for 100 ms with a configured 1 MiB request allowance).

Returned these tests plus bounded queue/task cleanup, typed schema/missing-value
semantics, AST dependency fencing, cached native expressions, explicit invalid
filter outcomes, and bounded streamed result ownership to the existing owner.
No query integration until these behavioral gates pass.

## M1C accepted and memory foundation assigned

Accepted owner `aa7621b` as main `ac361e1`. Reviewed reservation ownership through
late rejection/archival and explicit capacity-charged cleanup failure. Primary
reran 42 tests/typecheck, then installed locked bridge dependencies on main and
ran `mise run check:bridge`: 42 tests, typecheck and production build passed.
`mise tasks validate` passed. Earlier live proposal and remote-stop evidence stand.
The public SDK still has no permission-resolution API; this limitation remains
explicit rather than introducing private protocol behavior.

Reused bridge implementer for fresh `impl/memory-foundation` from reviewed main,
ownership ONLY `crates/lvu-memory/**`. Bridge ownership returns to primary.
Assignment covers versioned TOML canonical recipes/sources, SQLite working state
and revisions, draft/applied separation, stable identity, bounded similarity
retrieval and crash/concurrency tests. UI, AI, capture retention and disk eviction
are outside that isolated package.

## Runnable preview policy

User requested periodic runnable versions while implementation continues without
waiting for feedback. Published local immutable preview 001 from UI candidate
`852a855`, after primary reran ten Rust tests and full PTY workflows successfully.
`mise run preview` launches the synthetic TUI demo; no production adapter claim.
Manifest records source hash/checksum. Details and future checkpoints are in
`docs/previews.md`. Full UI code review/integration continues separately.

## Discovery accepted

Accepted owner `e4021c8`: primary reviewed corrected identity, bounded subprocess
lifetime and enumeration, reran 11 owned tests and all three external regressions.
Integrated into root workspace and reconciled lockfile. `mise run check:rust`
passed on rerun. First broader run exposed an existing capture-test readiness race
(empty PID file parsed); correction assigned to capture runtime owner. Live Docker
remains untested here due socket access, recorded Docker tests passed. No UI claim.

## TUI shell accepted and search direction

Accepted owner `852a855` after primary review of layout-before-sync, per-view
generation fencing, nonblocking dispatch and bounded completion processing.
Ten UI tests and full real PTY passed independently and after root integration.
Workspace run uncovered intermittent discovery fixture ETXTBSY, assigned to its
owner; this is recorded rather than claiming an entirely green integration run.
Added mise build:tui and test:pty and reconciled the root lockfile.

User clarified default filtering should be simple text search with advanced
filtering added as needed. Updated contract; next UI chunk implements independent
literal search and optional advanced constraints, with a runnable demo checkpoint.

## Discovery fixture stabilization verified

Reviewed follow-up `4ae7fc1`: Docker script fixture lifecycle is serialized only
in tests, avoiding inherited writable executable descriptors across parallel
forks. Primary reran the discovery suite, folded the fix into the discovery
logical commit, and reran full `mise run check:rust`: all 38 Rust tests, formatting
and clippy passed. This does not erase the separately assigned capture PID-file
readiness race; that correction remains with the runtime owner.

## Capture runtime review and live adapter assignment

Candidate `6e5cc206`: primary reran 13 runtime tests successfully. Independent
`/tmp/lvu-review-runtime` tests reproduced permanent start reservation after a
cancelled start, overwriting future metadata schema, and abort exceeding its
deadline while a delayed writer finishes. Returned scoped lifecycle, metadata,
catalog recovery and diagnostic corrections to the capture owner. Not integrated.

Assigned the freed discovery implementer a separate `impl/live-row-provider`
branch owning ONLY `crates/lvu-live/**`. It may use the provisional runtime commit
as a dependency but cannot edit core/runtime/UI. It builds bounded asynchronous
journal paging/index/cache behind the accepted synchronous RowProvider seam.
Primary accepts only its new adapter commit after runtime acceptance; no copying
of provisional runtime into main. This permits real-source integration work to
proceed while lifecycle fixes are reviewed.

## Production query review round 2

Candidate `48ee6ca`: primary independently reran 20 query tests and four adapted
external regressions, all passed. Reviewed bounded host IO/lifecycle, cached native
expressions, synchronized generations, result sinks and literal text search.
Two new external schema regressions failed: first-null numeric field is wrongly
String and rejects numeric comparison; string-first numeric conflict is silently
stringified instead of null with provenance. Requested those narrow corrections
and consistent failed-stage dependency fencing for color predicates. Other scope
frozen for integration; no new live provider proof requested.

## Memory foundation review round 1

Candidate `d7e6bb`: all 13 tests independently pass. External
`/tmp/lvu-review-memory` reproduces two consistency failures: an immutable revision
conflict is detected only after canonical TOML overwrite, and reopening recipes
rolls back a newer source definition to an old embedded source snapshot. Returned
preflight/transaction/lock ordering, source authority separation, bounded file
reads, search-limit alignment and explicit import semantics to the memory owner.
No memory integration until canonical and working-state consistency gates pass.

## Text-search preview accepted

Accepted owner `5a3974c` as main `e07bd8d`. Reviewed literal search, per-view
editor state and debounced request seam. Primary reran 13 TUI tests and real PTY
search/clear/arrival workflow after integration. Published immutable preview002
and advanced previews/latest;001 remains unchanged. Full production search plus
advanced-filter interleaving will be validated when native query adapter is wired.
