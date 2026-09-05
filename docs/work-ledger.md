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

## Production query foundation accepted

Accepted owner `71d8232`, reviewed final null/conflict/color dependency corrections
and reran all six external regressions successfully. Integrated lvu-query into
root workspace and reconciled its lockfile; global mise RUSTFLAGS preserves the
existing no-debug-info build policy across root and expression tasks.

Primary `mise run check:rust` passed all 64 Rust tests, formatting and clippy.
`mise run check:expr` passed 20 Python tests, three spike tests, 56 differential
cells and malformed/bounded protocol checks. Task validation passed.

Accepted scope: compiler host, native literal search and advanced batch predicates,
additive scalar enrichment, tolerant typed scalar projection and bounded-result
publication seam. Nested JSON projection, multiline grouping, arbitrary-command
enrichment, disk membership/index integration and TUI query wiring remain work.
Helper descendant cleanup is Unix-specific; no cross-platform completion claim.

Runtime candidate `13207f7` passes all three independent regressions. Its owned
suite exposed read_page returning Closed when a queued request races Finish;
returned bounded read-only fallback plus admission-before-launch correction.

## Command enrichment assigned

Reused query implementer for fresh `impl/command-enrichment` from reviewed main,
owning ONLY `crates/lvu-command-enrich/**`. Query/helper/spike ownership returns
to primary. Assignment implements bounded persistent JSONL command enrichment,
stable-ID correlation, independent per-record outcomes, protected-field rules
and subprocess failure/timeout tests. No Polars dependency, UI or storage edits.

## Capture runtime accepted

Accepted owner `9f117e5`: primary reviewed admission-before-acquisition barrier
and read-only paging fallback across writer termination. Independently reran
19 runtime tests and all three external lifecycle/metadata regressions. Added
ingest to root workspace and reconciled lockfiles; `mise run check:rust` passed
all 83 Rust tests, formatting and clippy after integration.

Shared source capture, progressive partial records, durable bounded journal
paging, graceful drain and explicit incomplete abort are now integrated. Runtime
manager closes admission on shutdown; exact persisted source definition equality
remains required until explicit source replacement is implemented. HTTP/restart
execution remains unsupported. Live adapter owner received the accepted API.

## Memory foundation accepted

Accepted owner `47c9ce4`. Primary reviewed directory-wide publication locking,
revision preflight, current-source authority and canonical imports. Updated the
external harness dependency pin and reran both consistency regressions; passed.
Integrated into root workspace and reconciled lockfiles. Full root check:rust
passed all102 Rust tests, formatting and clippy.

Memory is a library foundation, not yet connected to the TUI. SQL retrieval
returns/scoring are bounded but total query work still depends on metadata size;
this limitation remains documented. Source snapshots in recipes establish missing
identities but no longer overwrite current source configuration.

## Composite search/filter review

Candidate `155c26f`: primary15 TUI tests and PTY pass. Independent
`/tmp/lvu-review-composite` reproduces advanced-then-search falsely accepting an
unexecuted invalid advanced filter: latest search request contains both constraints
but fixture validates only its purpose. Returned full-constraint validation and
failure-specific rebase, with both submission orders (not only completion orders)
required. Candidate not integrated; immutable preview002 unaffected.

Accepted replacement `487b986`: primary independently reran the external
advanced-then-search regression, all 16 TUI tests, real PTY workflows, and clippy
with warnings denied after integration. Complete composite constraints now carry
structured failure identity and preserve the last applied view across rebasing.

## Live-source preview integration

Live adapter candidate `8d6c1c8` remains under review. Primary identified viewport
starvation during indexing, loss of paging after the index cap, incomplete-page
index recovery, overlapping worker ownership, blocking disk operations, and
decoded invalid-byte cache sizing. Returned to the discovery/live owner for
correction and acceptance regressions; no main integration yet.

TUI owner now owns `crates/lvu-app/**`, minimal generic terminal hooks in
`crates/lvu/**`, and real-source PTY tests. A separate composition package avoids
the dependency cycle between the UI library and live adapter. First real-source
preview targets file/command acquisition, live paging, and graceful shutdown.
Native query integration remains explicit pending work; preview002 stays intact.

## Command enrichment review

Candidate `da255560` passed its six subprocess tests but is not accepted. Returned
unbounded attempted-ID tracking, timing-based completion, missing pending-event
diagnostics, swallowed extra-output failures, and post-allocation input bounds.
The query/command owner is correcting only `crates/lvu-command-enrich/**` and
must propose an explicit completion protocol rather than infer it from silence.

Replacement `548aa4e` adds framed completion and bounded caller-owned attempts.
Primary reran all six owned tests successfully, then reproduced two failures in
`/tmp/lvu-review-command`: oversized serialization consumes IDs before any send;
total output overflow after the final event returns Ready without a diagnostic
or completion marker. Returned these plus restoration of blocked-write,
cancellation, stderr-flood and descendant-cleanup tests lost in the rewrite.
Still not integrated. Framed protocol is accepted in principle; durable attempt
ownership and batch failure semantics must be explicit before publication.

Accepted command foundation `201422f`: both independent regressions and all seven
owned tests rerun successfully by primary, including restored lifecycle coverage.
Root workspace integration removes its standalone lockfile/profiles. Application
durable attempt/result storage remains a required integration responsibility.

Live replacement `0cee521` passes all 13 owned tests independently. External
`/tmp/lvu-review-live` reproduces unreadable history after reopening with a smaller
page-byte budget. Returned cache-format/config compatibility, quadratic decoded
truncation and unconditional per-page idle delay for narrow correction.

Accepted live adapter `6d94e2a`: primary independently reran all 14 owned tests
and the page-budget reopen regression, then repeated owned tests after root
workspace integration. Page geometry is part of the cache format, projections
truncate decoded text in linear work, and backlog indexing yields without idle
sleep. Root owns the reconciled manifest and lockfile.

Real-source app candidate `0db1ef0` is under review. Returned startup-error cleanup,
source admission before acquisition, duplicate reuse, async dialog result/draft
ownership, and byte-preserving path identities. No preview publication yet.

Query owner now owns only new `crates/lvu-view/**` for nonblocking native live
queries and disk-backed match membership. Reuses accepted Polars query engine,
literal TextSearch, composite UI revisions and durable SourceHandle batches;
no alternative production text engine. Root manifests, lvu-app and UI are not
part of this assignment. Shared Polars build target remains exclusive to this
owner while the primary runs lightweight live/app checks.

Accepted app `eb5547c`: reviewed universal startup cleanup, admission before start,
duplicate reuse, async request/draft identity and byte-preserving path hashing.
Integrated new lvu-app composition package without creating a UI/live dependency
cycle. Primary reran 18 UI and two app tests, demo PTY, real-source PTY and
startup-failure cleanup. Root mise now exposes build:app and test:pty:real.
Preview003 publishes actual capture/display with native queries explicitly
unwired. Reopening files currently recaptures contents; cross-run acquisition
deduplication is a remaining runtime/product concern documented for testers.

## Delivery focus after advisor review

Continue without waiting on user feedback. Query owner is simplifying unaccepted
e3070cb: bounded in-memory match sequences, immutable publication and incremental
offset processing, keeping existing native TextSearch semantics and UI seams.
No additional disk membership actor or spill-format work before usable search.
TUI owner now wires that API directly into lvu-app with real narrow/append/clear
PTY acceptance; it owns UI/app/PTY and display-only timestamp formatting in live.
Capture owner owns core/ingest file resume to avoid recapturing unchanged files.
Those assignments are isolated; no new foundation packages are planned.

Discovery candidate f606c92 accepted after primary reviewed explicit source
selection, bounded scans and authoritative definitions. Primary19 UI+3app tests,
demo PTY, actual discovery/capture PTY, clippy and fmt pass. Preview004 publishes
discovery while search implementation continues; prior previews stay immutable.

Accepted file resume `7c9bc18` after validating acknowledged-prefix integrity
before stale-cursor reconciliation. Primary reran 18 core, 24 ingest, and 14 live
tests plus real-source PTY on the integrated tree. Unchanged files resume without
duplication and suffixes preserve raw bytes. A follow-up removes duplicate prefix
validation on clean reopen; this performance work is isolated to core/ingest.

Accepted native adapter `4fb53e3`: reduced in-memory immutable membership,
incremental fixed-boundary scans, lazy advanced compilation, and applied
high-watermarks isolated from candidate progress. Root integrated its workspace
manifest. Accepted app `d1c9f1e` composes this adapter with real sources, readable
UTC capture timestamps, and real search/advanced/arrival/clear PTY coverage.
Primary independently reran 19 UI, four app and four native-view tests. Preview005
publishes this product workflow with durable file resume. Path completion remains
an isolated active UI assignment; no user feedback gates continued development.

## Active acceptance: path completion and resume performance

Path completion candidate `d30a568` adds asynchronous bounded enumeration and
real PTY file capture. Primary review returned dialog-lifetime request fencing,
repeat-Tab directory continuation, and directory-symlink handling for correction.
UI/app ownership remains with the TUI implementer; no preview006 published yet.

Resume optimization `20a524e` is provisionally integrated but not published.
Primary core18 and clippy passed; runtime24/25 exposed an AlreadyRunning race
when reopening immediately after terminal status. Capture owner is correcting
lifecycle ordering and adding a deterministic regression before acceptance.
Preview005 remains immutable and points to its tested prior source commit.

Path completion `5e7313e` reviewed and integrated: app-global request IDs,
consumed unique results, directory symlink continuation. Primary 19 UI tests,
five app tests, demo PTY, real completion/capture/search PTY and clippy pass.
Resume fixup `4b2ab28` releases the runtime lease before publishing terminal
progress; folded into the unpublished optimization topic. Combined primary validation passed: 18 core, 25 runtime, four native-view tests,
real completion/search/capture PTY, and core/ingest clippy. Preview006 accepted.

Next assignment: memory owner e66a7d58 now owns UI/app/PTY plus narrow memory API
additions for real working-view restore, coalesced off-thread autosave and recent
sources. TUI completion owner is finished. No root/mise/preview ownership granted;
heavy target reserved to primary until preview006 checks complete. No named
recipe editor or AI expansion in this slice.

## Working-memory app acceptance in progress

Candidate `f3eb5e8` provisionally integrated with root Cargo.lock reconciliation.
Primary reran 20 UI plus fixture, eight app and 19 memory tests and the actual
restart/recent-source PTY; all pass. Review returned slow-load/user-edit fencing,
last-draft capture during pending queries and shutdown, reliable bounded worker
completion delivery, truthful save/flush outcomes, and same-session recent-source
refresh. Owner is correcting these within the same unpublished topic. No
preview007 yet; preview006 stays available and unchanged.

Memory correction `0ee67c5` applied to the unpublished topic. Primary independently
passed 21 UI plus fixture, 11 app and 19 memory tests; real restart/in-flight-draft
PTY, demo PTY, clippy and formatting. Load fences, reliable completions, acknowledged
saves and final-state flush are reviewed. One narrow pending-save cancellation
case remains: durable A -> queued B -> current A must remove B before debounce.
Owner is correcting it before preview007. Shared build target released to owner.

Accepted final working-memory correction `292447e`: pending B is removed before
state deduplication when the current state returns to durable/in-flight A. Folded
all owner corrections into one app-memory commit. Preview007 includes the reviewed
restore/save behavior, with independent primary tests recorded in preview notes.

## Structured presentation acceptance

Candidate `4ae9938` provisionally integrated; root lockfile reconciles direct
lvu-live serde_json dependency. Primary baseline passed 22 UI plus two unit tests,
12 app, 15 live and 20 memory tests, plus real pins/color/restart PTY. Review
returned populated-v1 migration compatibility (TEXT default read as BLOB) and
field-picker viewport/hitbox/selected-event stability. SQL probe confirms the
original migration gives existing rows TEXT values. Owner is correcting these
before preview008; preview007 remains immutable. Shared target released to owner.

Structured presentation correction `01f367a` applied: BLOB migration default with
populated v1 regression; stable picker event identity, viewport scrolling,
selection reset, clipped rows and corresponding mouse regions. Primary integrated
acceptance runs before preview008 publication. Root owns this publication and
keeps the previously shipped previews immutable.

Preview008 acceptance passed: primary 23 UI plus two unit, 12 app, 15 live,
20 memory tests, both PTY suites, fmt and targeted clippy.

## Native enrichment acceptance

Candidate `d8b8e3e` provisionally integrated. Primary baseline passed 24 UI plus
two unit, 12 app, 15 query plus eight host, five view tests and real enrichment/
filter/pin/restart PTY. Review returned runtime-stage failure handling, exact
scalar string projection, pending-enrichment restoration tracking, output-name
limits, and three-constraint rejection/rebase coverage. Owner is correcting the
same topic; preview008 remains unchanged and preview009 is not yet published.
Shared Polars target released to owner for corrective validation.

Enrichment correction `9900037` applied. Primary 26 UI plus two unit, 12 app,
16 query plus eight host, six view tests and actual enrichment/restart PTY pass.
Reviewed runtime candidate rejection, exact scalar strings, restoration pending
state and three-constraint rebasing. Returned one new failure-path issue: failed
live enrichment must not insert all IDs past a dependent advanced/literal filter;
unsupported output dtypes must report an error rather than silently becoming null.
Owner is making narrow corrections; preview009 remains unpublished.

Final enrichment correction `580ca4c` applied: unsupported projected output types
are explicit errors; dependent predicate failure adds zero new members rather
than bypassing active constraints. Accepted unchanged enrichment is recognized
across composite revisions. Primary integrated checks run before preview009.

Preview009 primary acceptance passed: 26 UI plus two unit, 12 app, 16 query plus
eight compiler host, seven view tests, real and demo PTY, fmt and clippy.

## Named-view acceptance

Candidate `aaf3e3f` provisionally integrated. Primary passed 28 UI plus two unit,
13 app, 20 memory and seven view tests plus actual shared-command/independent-view/
rename/restart PTY. Review returned user-rename fencing before delayed restore,
duplicate rename validation, and default-view admission before launching new
sources (including pending start reservations). Owner is correcting the same
unpublished topic. Preview009 remains unchanged; no preview010 published yet.
Shared build target released to owner.

Named-view correction `dd2ae4a` applied: user rename increments interaction state,
restored names remain within the whole-view fence, duplicate names are rejected,
and source starts reserve default-view capacity before acquisition. Failed
registration rolls back adapter ownership. Primary integrated release checks
run before preview010 publication.

Preview010 primary acceptance passed: 29 UI plus two unit, 14 app, 20 memory,
seven native-view tests, both PTY suites, fmt and targeted clippy.

## Agent handoff integration

Bridge host `20c214b` accepted after lifecycle correction: generation-gated
mutation, startup status before readers, owned Unix process-group cleanup,
tracked bounded worker joins, capped request serialization, immediate oversized
output rejection, and response/event distinction. Primary independently ran all
10 host tests, app clippy and formatting after root lockfile reconciliation.
The host is integrated as a module; no AI UI or new preview is claimed yet.
Snapshot exporter owner f458c358 has the shared target again. Primary will compose
export and host APIs into a reviewable agent workflow after export acceptance.

### Snapshot review and first Ask AI workflow

Snapshot candidate `ca2c238` remains unintegrated. Primary review returned four
concrete corrections: preserve the applied enrichment interpretation when export
batch geometry or selected rows differ, use accepted generation metadata, move
output-directory initialization off the UI call, and reject incomplete reads
before the frozen source boundary. Owner f458c358 retains `lvu-view/**` and narrow
query export helpers and owns the shared build target for corrective validation.

TUI owner 66889b2a is assigned the first filter/enrichment Ask AI workflow on a
fresh branch from `d149bde`. Scope is UI, app composition (excluding the accepted
bridge host), and PTY tests. It uses the snapshot and host APIs, previews typed
proposals, and applies them through existing native expression validation. Late
responses must respect the originating view and user edits; cancellation/offline
failures must preserve browsing. The owner may use the export candidate as a
provisional dependency, but cannot include it in the UI topic commit. Compilation
waits for the explicitly allocated shared target; implementation proceeds now.

Preview010 remains the latest published build. Neither backend acceptance nor a
mocked UI test alone constitutes a completed agent workflow.

Snapshot correction `2a7ed06` provisionally integrated with the root lockfile
reconciled for Parquet. Primary passed 11 view tests, 16 query tests, eight compiler
host tests and the bounded Parquet writer test. Review identified a remaining
incremental replay case: fixed page geometry does not preserve the actual batch
boundaries from separate live refreshes. An initially successful strict enrichment
must remain successful in the snapshot when a later batch fails. Owner is adding
that regression and correcting replay; the public export API remains unchanged.
The shared target is released to the query owner for this focused validation.

Snapshot correction `a54ba81` integrated: immutable membership retains accepted
batch boundaries and pre-evaluation schemas, charged to the membership cap.
Primary additionally records batch row counts rather than deriving them from
sequence IDs; durable reservation gaps are covered by a restart/export regression.
All 13 view tests pass serially; targeted clippy and formatting pass. Parallel
validation twice exposed a timeout in the pre-existing failed-candidate/live
refresh test; that remains an open test investigation, not a waived check.
The snapshot-specific tests passed in those runs. UI owner now receives the shared
target; query owner will diagnose the parallel failure without racing that build.

Query fixture correction `2fa6bb4` integrated. Partial-line capture under scheduler
load invalidated the test's exact record-count assumptions. A single burst write,
deferred fixture-only partial flush, and explicit capture-boundary assertions
preserve the publication checks. Owner passed the full parallel suite plus ten
additional runs (130 tests); no production changes were needed.

Ask AI candidate `f7ce2e9` provisionally integrated. Primary passes 32 UI state,
two UI unit, and 25 app tests with normal parallelism. Owner also reported both
PTY suites and one completed real local-provider proposal. Review returned
absolute dataset paths for default/relative capture roots, moving dataset metadata
work off the UI thread, tracked session-record writes, known-session cancellation
and shutdown error reporting, reuse to avoid exhausting bridge session capacity,
and protection for newer unfinished drafts. UI owner has the shared target for
corrections. Preview011 remains unpublished until those integrated checks pass.

Ask AI correction `bc2fe4f` applied provisionally. Primary passes 33 UI state,
two UI unit and 26 app tests under normal parallelism. Absolute snapshot paths,
off-UI context preparation, session reuse and unfinished-draft fencing are in
place. Final lifecycle review returned cancellation on application exit (including
pending session creation), interpreting remote cancellation result fields rather
than treating any successful RPC as a stopped agent, and bounding outstanding
session-record workers. Owner is correcting those narrow cases before preview011.

Final Ask AI correction `d744568` applied: shutdown settles pending creation and
requests remote cancellation before closing the bridge, validates remote lifecycle
status, and caps outstanding session-record jobs at four. PTY navigation now waits
for actual focus transitions. Primary passed 33 UI state plus two unit tests,
28 app tests, 13 view tests, and the complete real-source PTY workflow. Final demo,
formatting and lint checks precede immutable preview011 publication.

Preview011 published from `f8f360d` after primary real-source and demo PTY,
formatting and clippy passed. Previous previews remain immutable.

## Freeform investigations

Candidate `9266081` provisionally integrated: separate local session, fixed
Parquet handoff, follow-up questions and explicit saved-session resume. Primary
passed 34 UI state plus two unit tests and 29 app tests. Owner reported both PTY
suites and a controlled real-provider resumed investigation identifying the
fixture's database timeout and request ID.

Review returned generation-safe cancellation, settlement of pending resume,
retaining ownership after incomplete cancellation or ambiguous prompt failure,
terminal-event fencing, bounded/versioned owned-path metadata loading, and
merging late saved-list results without hiding newly created investigations.
The owner has the shared target for corrections; preview012 is not published yet.

Investigation correction `e3a5512` integrated: cancellation is generation-fenced,
pending resume is settled, incomplete cleanup retains ownership, and lost bridge
events cause visible failure and cleanup. Metadata reads are bounded and
version/path checked; delayed loads merge with newly created sessions and report
listing limits. Primary passed 35 UI state plus two unit tests, 32 app tests,
full real-source investigation/resume PTY, formatting and clippy. Demo PTY is the
last publication check for preview012; prior previews remain immutable.

Preview012 published from `8db376b` after the demo PTY also passed.

## Source definition assistance

Candidate `029649a` provisionally integrated. Primary passed 37 UI state plus two
unit tests and 33 app tests. Source proposals use typed definitions and explicit
review before the existing admission/capture path; empty startup is supported.
Review returned tracked/cancellable preparation workers, off-runtime blocking
filesystem work, a complete launch preview including environment/effective cwd,
early unsupported restart rejection, bounded atomic context publication, and
session metadata for reused contexts. Owner has the shared target for corrections.
Preview013 remains unpublished; preview012 stays available.

Source correction `a7d3764` integrated. Preparation retains one owned worker through
cancellation, filesystem work runs off the event loop, and manifests are capped
and atomically published. Review includes effective launch configuration, rejects
unsupported restart policies, and records reused sessions in new contexts.
Primary corrected the cancellation writer to return a non-retryable error rather
than Interrupted, with a regression covering write_all. Primary passed 37 UI state
plus two unit tests, 35 app tests, full real-source and demo PTY, fmt and clippy.
Preview013 is published after rebuilding the final binary; earlier versions remain
immutable.

## Named reusable recipes

Candidate `25dfce5` provisionally integrated. Primary passed 38 UI state plus two
unit tests, 35 app tests and 21 memory tests. Browser/save/import use the existing
memory worker and versioned TOML store; acceptance executes native expressions.
Review returned all-or-nothing recipe application on failed constraints, guarding
later presentation edits and queue admission, explicit incompatibility for every
unsupported setting, request/dialog fencing for asynchronous persistence, and
transactional duplicate-name checks. Owner has the shared target for corrections.
Preview014 is not yet published; preview013 remains available.

Recipe correction `64b12a4` integrated: failed application restores all accepted
constraints, presentation respects later edits, unsupported settings are refused,
and persistence responses carry request/dialog fences. Save captures accepted
configuration at click time; new-name/identity admission is inside the SQLite
write transaction. Primary passed 40 UI state plus two unit tests, 36 app tests,
21 memory tests, complete real-source recipe/restart PTY, formatting and clippy.
Preview014 publication follows the final demo PTY check.

## Fixed capture-time windows

Preview014 is published and remains available while this topic is reviewed.
Candidate `4336e2c` is provisionally integrated as `c57b3c6`. Primary passed
41 UI state plus two unit tests, 36 app tests, 21 memory tests and 13 native-view
tests. The candidate adds absolute half-open capture-time ranges, per-view memory,
recipe round-tripping and exact snapshot bounds; rolling and parsed event time
remain outside this slice.

Review returned UTF-8-safe strict timestamp parsing, time-edit restore/AI fences,
preservation of submitted composite constraints, and stable anchoring for the
around-selected action. The UI owner has the shared build target for corrections.
Preview015 has not been published.

Correction `b88c113` integrated: strict ASCII grammar precedes timestamp slicing,
time edits fence stale restoration/AI, around-selected uses the opening event ID,
and pending time/advanced constraints survive either order and invalid advanced
rebasing. Primary passed 46 UI state plus two unit tests, 36 app tests, 21 memory
tests, 13 native-view tests and the complete real-source PTY. Demo, formatting and
clippy also passed; preview015 is published from this topic. A test-only assignment will
replace scheduling assumptions in the bridge EOF and memory queue fixtures.

## Deterministic bridge and memory fixtures

Owner `199d9dd` replaces the EOF request-admission race with explicit child
handshakes and the memory fixture's fixed sleeps with rendezvous channels.
Both focused tests passed 100 consecutive owner iterations; the full parallel
36-test app suite passed. Primary added bounded event receives so missing worker
delivery fails the test rather than hanging, and moved sender joining after the
load result. These are test-only changes; preview015 remains immutable.

## Rolling capture-time windows

Candidate `e83e429` is provisionally integrated as `ab75da4`. Primary passed
48 UI state plus two unit tests, 37 app tests, 21 memory tests and 13 native-view
tests. Preview015 remains current.

Review returned clock refreshes cancelling unfinished scans, refreshes superseding
pending recipe transactions, repeated compilation of unchanged expressions, and
wall-clock rollback handling. Requested native journal expiry and snapshot tests,
delayed completion/recipe tests, and compilation-count evidence. The UI owner has
the shared target for corrections; preview016 is not yet published.

Correction `c6c3de3` integrated. Pending queries/restores/recipes defer clock ticks;
latest time coalesces without cancelling work. Monotonic cadence handles backward
wall-clock changes. Unchanged native expressions reuse compilation; time-only
membership scans restart their schema/checkpoints safely. Primary passed 51 UI
state plus two unit tests, 37 app tests, 21 memory tests, 14 native-view tests and
the complete real-source PTY. Actual-journal coverage verifies expiry, one compiler
call across revisions, and exact snapshot bounds/membership. Refreshes still scan
retained history in bounded batches; this limitation is documented for preview016.

## Recognized event-time windows

Candidate `dfb18fc` provisionally integrated as `78b9722`. Primary's combined
UI/app/memory/live/view tests passed. Review returned logfmt timestamp recognition
depending on clipped display fields and escaped-quote handling, plus event-time
diagnostics overwriting enrichment errors. Requested shared bounded full-record
recognition and regressions beyond 32 fields, quoted message content, and combined
diagnostics. UI owner has the shared target; preview016 remains current and
preview017 is not yet published.

Correction `4f2c001` integrated. Full bounded logfmt traversal no longer depends
on display clipping and skips escaped quoted message contents correctly. Combined
diagnostics retain enrichment failures alongside event-time missing/invalid counts.
Primary passed 52 UI state tests, 37 app tests, 21 memory tests, 14 live integration
tests plus three projection tests and 16 native-view tests. Full real-source PTY
passed. Demo, formatting and clippy also passed; preview017 is published from
this topic, with earlier preview binaries unchanged.

## Reversible multiline presentation

Candidate `cbff617` provisionally integrated as `dbe9f72`; primary combined
UI/app/memory/view tests passed. Review returned the custom regex subset,
linear group lookup and cache residency requirements for full group rendering.
Primary authorized the already-locked `regex = 1.13.1` package dependency and
updated the root lockfile. Requested standard bounded regex compilation,
binary group lookup, tiny-cache/invalid-UTF-8 rendering regressions and explicit
oversized-record behavior. UI owner has the shared target for corrections;
preview017 remains current and preview018 is not published.

Correction `0d40db4` integrated with the primary-owned regex lockfile update.
Standard bounded Rust regex replaces the subset interpreter; group lookup uses
binary search and worker-owned budgeted projections remove raw-cache residency
requirements. Oversized leading records remain visible with a soft-limit label.
Primary passed combined UI/app/live/memory/view tests, including 20 native-view
integration tests and two view unit tests. Real-source and demo terminal workflows,
formatting and clippy passed; preview018 is published. Semantic multiline events remain outside this
display-only grouping slice.

## Storage usage and disposable-index cleanup

Candidate `8223fa9` provisionally integrated as `62e34d6`; primary UI/live/app
tests passed. Review found filename-only deletion ownership, symlinked-parent
traversal, silent scan truncation, unbounded directory traversal/shutdown joining,
and cleanup rescanning beyond the reviewed candidates. Requested real index
header/identity validation, stable directory/file ownership through deletion,
review-bound candidates, bounded worker settlement and preservation regressions.
UI owner has the shared target for corrections. Preview018 remains current;
preview019 is not published.

Correction `36de42f` provisionally integrated and primary UI/live/app tests passed.
Reviewed candidate identities and structural index validation replace filename-only
ownership. Further review found mutable directory length in the identity, remaining
pathname replacement races at unlink, and uncancellable validation under the
registration gate. Requested stable directory handles/identity, handle-relative
cleanup with cross-provider coordination, cancellation-aware bounded validation,
and deterministic replacement tests. Preview019 remains withheld; UI owner has
the target for this narrow correction.

Correction `1e91bd9` integrated, including the primary-owned libc lockfile entry.
Directory identity uses device/inode; reviewed files include revision metadata.
Linux descriptor-relative enumeration/exchange/unlink and persistent provider
ownership locking replace pathname-only cleanup. Validation is cancellation-aware
and capped at 16 MiB, with larger artifacts preserved. Primary combined UI/app/live
tests, formatting, real-source/demo terminal workflows and clippy passed.
Preview019 is published; unsupported platforms refuse cleanup explicitly.

## Command palette (user-requested)

Palette owner has prepared a catalog/state/render module and deterministic tests
in two new files. Primary reviewed the API and requested complete semantic action
coverage and refreshed availability before execution. Ctrl-P is the intended
toggle, with live search, shortcut labels, Tab completion, keyboard/mouse selection
and disabled-context explanations. Existing operation handlers retain confirmation
and admission behavior.

The palette owner may validate only the lightweight `lvu` package using
`/tmp/lvu-palette-target`; the recipe-suggestion owner retains the heavy shared
target. Primary owns terminal overlay/module integration. Palette input must never
leak to an underlying editor; closing preserves its focus and draft while ingestion
and asynchronous progress continue. A real PTY story will verify open/search/Tab/
execute, disabled operations, Escape restoration, resizing and continued capture
before the feature is published.

Owner `c6d0893` integrated. Primary wired the overlay into the existing terminal
loop, keeping background ingestion/query progress active and routing palette input
without changing the underlying editor focus. Context refresh precedes execution;
paste is bounded, and shortcut labels precede descriptions for visibility.
Lightweight library tests and demo PTY passed. New palette PTY covers Ctrl-P,
search, Tab completion, execution, disabled cleanup, bracketed paste, editor
restoration and quit. The test uses the established 88-column geometry after pyte
hit a wide-character screen-buffer error at 110 columns; broad emulator validation
remains separate. Full live-app validation awaits the shared target release.

Full live-app palette validation now passed: 11 palette tests, 55 UI state tests,
39 app tests, full real-source PTY and a dedicated palette workflow with continued
file arrivals. Primary cleared only local workspace package artifacts after the
shared target reused stale suggestion-branch enum metadata; rebuilt validation
passed. Added `mise run test:pty:palette` and Ctrl-P help text. Formatting and
clippy passed. Preview020 publishes the palette independently of pending recipe
suggestion review.


## Preview021 — similar-source suggestions accepted

Integrated owner e83e42a after review corrections: lexical display evidence cannot
claim native scalar types, sample absence is qualified, empty samples retain
evidence, and save context remains bound to the submitted view. AI adaptation
explicitly changes only the advanced filter and refuses unsupported settings.
Primary reran 58 UI, 11 palette, 39 app and 22 memory tests, full real-source PTY,
live palette PTY, formatting and clippy. All passed. Publishing preview021; next
vertical is discovered field/value completion in expression editors.


## CLI stdin and heart presentation — assigned

User requested positional file sources, explicit command flags, stdin sources,
pixel-art startup branding (heart + lvu / love you), and a beating-heart activity
indicator. Preserve the in-progress expression-completion topic independently.

Capture owner 0362 owns core/ingest injected-reader support and lifecycle tests.
UI owner 668 finishes completion, then owns CLI composition and real controlling-
TTY/pipe tests. Presentation owner e66 owns only new delight module/tests; primary
will wire it into the terminal loop and publish. Shared heavy target remains with
UI owner; capture and presentation use existing lightweight targets only.

CLI acceptance: positional non-UTF8 paths, --file compatibility, --command/-c, --
option terminator, explicit '-' / --stdin and automatic nonterminal stdin; one
stdin acquisition per process with a fresh identity. EOF retains captured history
and UI, while early quit cancels an open pipe without hanging. Preserve exact bytes
and keep keyboard input separate. Pinned Crossterm 0.29 already selects /dev/tty
when stdin is nonterminal; verify that actual path with controlling-PTY tests.

Presentation acceptance: brief skippable pixel-art splash, no sleep or acquisition
delay, first key still works, bounded monotonic animation, truthful busy/idle/error
heart with readable status, tiny-terminal and reduced-motion support. No images
or extra rendering engine required.


## Preview022 — heart presentation and sampled editor completion

Accepted presentation module c9a6c11 and editor completion c2dd9ff. Primary wired
startup/footer without sleeps, retained first-input handling, capped animation at
8 Hz, and added reduced-motion/disable switches. Independent read-only review
found narrow-footer crowding and false activity on view switches; both corrected.
Internal view UUID was removed from footer to preserve useful active-filter status.

Primary corrected control-character insertion from Rust-style escapes to valid
Python Unicode escapes and escaped popup labels to preserve row geometry. Help
height now exposes its mouse instructions. A restart PTY had depended on the
transient unfiltered frame; it now asserts the persisted ERROR filter directly.

Validation: 60 UI, 11 palette, nine delight, three unit and 39 app tests; full
real-source PTY (including completion/native apply/restart), real palette PTY,
demo PTY, dedicated startup/first-key/expiry/idle/arrival/resize/reduced-motion/
disabled/restoration PTY, fmt and clippy. All final checks passed. An initial
extra draft-filter step encountered pyte's known orphan wide-cell display issue;
the dedicated startup test now tests dismissal directly, with filtering covered
by the full integration suite. No terminal portability claim beyond these checks.

Publishing preview022. Stdin backend a5b81c3 has returned for separate review;
CLI owner composes positional files, flags, and actual controlling-TTY pipes next.


## Settings, themes and cache budget contract — in progress

User requires all settings to map to TOML. Authoritative global path will be
$XDG_CONFIG_HOME/lvu/settings.toml, falling back to ~/.config/lvu/settings.toml.
The settings UI
reads/writes that file, not an independent SQLite/JSON preference copy. SQLite
continues to own operational view history. Example: settings.example.toml.

Sections: paseo provider/mode/thinking; appearance theme and delight preferences;
cache.memory row and query-membership budgets; cache.disk aggregate and per-source
derived-index budgets. Aggregate default proposed 5 GiB, per-source 256 MiB. Raw
captures and investigation exports remain durable and excluded from cache eviction.
Limits take effect at restart initially; saved versus effective values and process
environment overrides must be explicit. External TOML edits load on next launch.
Unknown/malformed/future settings must fail visibly without silently overwriting.

Owner f458 handles bounded TOML storage/schema in new app settings files. Owner e66
handles theme rendering only. Owner 48ae handles actual aggregate derived-index
budget enforcement in lvu-live. UI owner 668 finishes CLI/stdin before settings
composition. Primary owns shared manifests, documentation and final integration.
No ignored global-budget knob may be published as implemented.


### XDG correction

User corrected settings placement: preferences are global XDG configuration, not
owned by a capture directory. Settings owner and UI composition owner notified.
Cache/data resolver defaults are respectively XDG_CACHE_HOME/lvu (~/.cache/lvu)
and XDG_DATA_HOME/lvu (~/.local/share/lvu). Relative/empty XDG values fall back.
Explicit capture-dir remains supported; existing captures are never silently
moved or deleted. Paths and actual budgets remain visible in settings/storage.


### CLI/stdin integration review

Backend a5b81c3 and CLI composition 0efeb387 provisionally integrated. Primary
reran 139 targeted tests and the controlling-PTY stdin story; passed positional
files/commands/pipe, regular-file input, exact bytes, keyboard separation, EOF
and early quit. One narrow correction is pending: arbitrary character devices
must not use uncancellable blocking reads, and shared pipe status flags must not
leak on teardown. Owner has the shared build target to validate that correction.
Added mise test:pty:stdin. Preview022 remains current until review closes.


### Preview023 acceptance

Accepted corrected backend 2a918a9 and CLI 0efeb387 + 6f5ed116. Primary also
made the Linux procfs pipe reopen nonblocking, with a named-FIFO/no-writer PTY
regression. This avoids waiting for a writer during attachment and isolates
O_NONBLOCK from parent-held descriptors. Arbitrary devices are rejected; /dev/null
is explicitly supported. Partial-read failures retain buffered bytes and report
an error state.

Final primary checks passed: 52 core/runtime tests, 40 app tests, prior integrated
discovery/memory/query suite, full real-source PTY, updated stdin/parent-flags/
FIFO/device/EOF/early-quit PTY, palette PTY, fmt and clippy. Preview023 is independent
of pending global XDG settings, themes, and aggregate disk-cache budget work.


## Settings/theme foundations integrated; composition pending

Settings owner 4abbedc integrated with pinned app dependencies. Primary added a
one-second bounded cooperative-save lock wait and regression so an occupied
global settings lock cannot hang the worker indefinitely. Eleven tests passed in
the lightweight harness referencing the actual in-tree sources; clippy passed.
TOML saves preserve existing invalid/future files and stale temporary artifacts.
Existing presence-based LVU_NO_DELIGHT/LVU_REDUCED_MOTION/LVU_ASCII overrides remain
compatible and explicitly attributed. Canonical saves do not preserve comments.

Theme owner 1e9ba11 integrated with public theme module. Primary reran the full
lightweight lvu suite, including six theme tests, and clippy. Semantic themes
remain Terminal by default until the settings screen supplies selected values.
No new preview yet; preview023 stays current.

Independent global-budget review of edfca51 found failure-path ledger/file size
divergence and conflicting provider caps. Returned to owner for corrections and
failure-injection/two-provider regressions. UI composition may continue against
the API but actual global-budget enforcement is still a release gate.


## Aggregate derived-index budget accepted

Owner correction 4d88139 integrated. Independent reviewer confirmed append and
rebuild reservations precede growth and failed mutations reconcile actual file
sizes under the ownership lock. Failed reconciliation disables further growth
conservatively. The ledger persists a shared cap: differing active providers
refuse growth; a changed cap is adopted after all writers close and reconciliation
succeeds. Capture and already-indexed history remain available at the limit.

Primary reran seven internal and 20 integration tests on the integrated tree,
including injected write/flush failures, differing caps, restart adoption, bounded
cache behavior and existing cleanup ownership tests. Root lockfile reconciled
for the added test-only UUID dependency. Explicit reviewed cleanup frees space;
no automatic raw-data eviction or unverified-file deletion was introduced.
The Settings composition owner has the updated API; preview023 stays current
until the complete settings/UI integration passes actual terminal workflows.


## Startup visual redesign — user override

User supersedes the brief/any-key splash: persistent video-game-style pixel-art
title screen, bold LOVE YOU LOG TIME lettering, large shaded heart with reflective
highlight. Esc enters the viewer; Ctrl-C remains an emergency quit. Other inputs
must not affect hidden editors. Acquisition/query ticks continue behind the title.
Footer heart becomes a compact shaded pixel badge with tasteful active motion.
Reduced-motion retains static artwork; disabling delight bypasses title entirely.

Presentation owner e66 owns only delight.rs/module tests. Settings UI owner 668
owns terminal modal routing while composing settings; primary will coordinate
test helper and dedicated title PTY changes. No changes to published preview023.
Acceptance includes persistent visibility beyond old timeout, exact title/prompt,
reflection/shadow colors, normal/large/tiny layouts, Escape-only entry, background
arrivals, ignored input, Ctrl-C cleanup and theme/reduced-motion compatibility.


Startup eligibility clarification: show the title only for a source-less launch.
Any CLI source intent (positional file, --file, command flag, explicit stdin, or
automatically detected redirected stdin) bypasses it, including empty/slow sources.
Derive this from parsed launch options rather than currently loaded views. Footer
delight remains enabled when only the title is skipped. Both owners notified.


## Reported discovery quality and selection bugs — priority fix

User reports SQLite/lock candidates and selection disappearing below the fold.
Primary confirmed process discovery admits generic writable regular files, while
project name.contains("log.") accepts non-log extensions. UI computes capacity
as one row per candidate but emits two lines plus arbitrary paragraph wrapping.

Discovery owner 48ae owns stricter evidence-based admission and artifact exclusions
with deterministic proc/project fixtures; real tee/stdout destinations and manual
source entry must remain useful. UI owner 668 owns fixed-height candidate rows,
full-width theme selection colors, separate selected details, viewport anchoring
and exact mouse hitboxes. Tests must cover long Unicode rows, >30 candidates,
small/resize geometry, last-row selection and correct launch identity. These
reported bugs take priority over expanding the feature surface.
