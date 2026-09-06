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

## Preview task usage and current UI requests

The preview task now uses an executable file task with Usage annotations and raw
terminal I/O. Task help documents positional files, repeatable file/command flags,
stdin and capture directory. The wrapper execs the immutable latest binary with
original argument boundaries; no evaluation of user shell text occurs in mise.
Primary verified empty arguments, spaces, literal shell metacharacters, repeated
flags, option termination and redirected stdin using a scratch binary. The
published binary is unchanged. Dash-prefixed paths can use `./-name`, or
`mise run preview -- -- -name` (one separator for mise, one for lvu).

User additionally requests subtle theme-colored dialog surfaces, clearly distinct
editable fields, a visible focused input cursor, and standard theme presets.
Existing theme owner extends semantic roles/presets; existing UI composition owner
owns render integration, Unicode/clipping-aware cursor placement and focus tests.
Enrichment editor terminology also needs to distinguish actual input columns
(`raw` and recognized source keys), the named output and before/after preview
labels. These are active UI work, not claims about preview023.

## Case conversion support and clearer expression errors

User correctly identified str.to_uppercase as row-local. Both Python and native
allowlists omitted case conversion; the generic rejection conflated unsupported
operations with cross-record operations. Added upper/lowercase to both paths and
changed generic function diagnostics to say not yet supported by lvu. Aggregate,
reverse and other cross-record restrictions remain intact.

Validation: 23 Python tests, 26 native query/host/library tests, native clippy,
80 whole/one-row-partition differential values/nulls/dtypes including Unicode
expansion, and actual PTY uppercase enrichment/details/raw preservation/quit.
The standalone compatibility lockfile was reconciled with the existing query
Parquet dependency, restoring locked offline runner builds. The initial PTY build
caught a stale cross-worktree lvu artifact; cleaning only lvu/lvu-app and rebuilding
resolved it before publication. No Polars cache was removed.

Discovery relevance correction c1e9e97 integrated as ae322e0. Primary reran two
unit and 13 integration tests, including FIFO replacement, tee capture, pipe
nonconsumption and irrelevant artifact exclusion. Selection rendering remains
owned by the active UI composition.

User wording: interface should use the brain emoji (Ask 🧠 / 🧠 settings), with
model and agent-session descriptions as needed. Paseo remains an implementation
detail, not a user-facing product label. UI owner notified; internal configuration
schema and protocol identifiers stay stable.

## Settings/dialog integration and iterative enrichment — active

Integrated XDG settings/runtime composition, standard themes, persistent title,
fixed-height discovery selection, and the padded dialog shell. The full composed
real-source PTY passed, including search/enrichment, source discovery, named views,
recipes, time windows, grouping, agent workflows, storage and restart. Dedicated
settings, title eligibility/modal behavior, stdin, palette, demo and activity PTYs
also passed. Primary corrected the storage scanner to inspect the provider's
actual XDG derived directory independently of the capture root; final settings
save failures now remain errors during shutdown. Final checks remain before the
next immutable preview; preview024 stays available.

Shared XDG caches exposed a source-ID collision across distinct capture roots.
Accepted live-owner f820181 as a7528aa: V3 derived headers and filenames bind to
the journal's first persisted acquisition UUID. Two managers can now concurrently
index distinct journals for the same logical source; V2 files remain safely
recognizable for cleanup. Integrated 8 live unit and 21 integration tests passed.

User explicitly requests accumulating successful enrichments. Current published
single-stage replacement behavior is not sufficient. Native/query owner and UI
owner are implementing ordered, stable-ID stages, explicit add/edit/remove,
whole-candidate validation and last-good rollback. Leading `/pattern/` shorthand
with named captures compiles each output to native Polars `str.extract`; later
steps may read earlier derived fields. Primary owns backward-compatible working
state and recipe composition. No successful old stage may disappear merely
because the user adds another extraction.

Gzip owner returned 0a9823c for review. Magic-based bounded multi-member decoding
and decoded resume are present, but graceful-stop/decoder cancellation and
fingerprint/file-identity consistency require correction before integration.
Primary gzip discovery admission and actual-file PTY are prepared separately.
EOF is static archive completion; gzip stdin is not in this task.

### Preview025 published; follow-up work continues

Published immutable preview025-settings-dialogs from9b91ad3. Final copied binary
passed the full real-source PTY and settings PTY. The rolling-window restart
story now waits for both restored constraints and asynchronously loaded matching
rows, rather than assuming row availability from the status label alone. Earlier
primary dedicated title, palette, stdin, demo and activity workflows passed.

Accepted corrected gzip topic0df4524 as96a5190. Integrated core/ingest tests:66
passed. Intentional stop no longer becomes a decode/truncation error, and resumed
prefix cancellation keeps the cursor unchanged. Primary app-level gzip PTY and
final publication remain pending.

User asks horizontal log-pane scrolling. Preview025 clips long event text and
has no horizontal shortcut. Primary will add per-view Left/Right event scrolling
and reset, with command-palette discoverability, after the stage UI topic lands.
Timestamp/level/pinned columns stay anchored; scroll changes presentation only.

Native stage topicfee34dd is under one focused correction: independent additions
must not fail solely because an unchanged accepted stage reports an error on a
later malformed record. Candidate-introduced/dependent failures must still reject
atomically. UI stage topic owns Add/Edit/Remove and native DTO convergence.
Primary has implemented working-memory/recipe mappings pending that DTO, including
explicit empty-chain semantics and exact editable regex sources in TOML.


## Primary integration: preview026 candidate

Integrated native ordered chain c215f049 as45e16f4 with UI99b0f567 as201d269.
Working-memory and recipe glue now preserve exact ordered extraction definitions,
stable IDs, unfinished edit targets, and explicit empty chains. Actual chain PTY
passed named regex outputs, dependent uppercase stage, failed-add rollback,
arrivals, and restart. Gzip PTY passed magic detection, plain .gz, multi-member,
exact decoded bytes and duplicate-free reopen. Horizontal event-only navigation
uses Left/Right and0, bounded Unicode display-column slicing and palette entries.

Search now parses raw/field literals, /regex/ims and explicit pl.col predicates.
The normal raw literal constructor remains available for callers requiring
uninterpreted text. Found publication/completion ordering race: native tick
published membership before new search enqueue used UI's older applied base.
Terminal now accepts completions before debounce enqueue. Debounce is300ms;
queue admission failure retains/retries final drafts, including empty clears.
Unit and real-PTY coverage exercises rapid edits, clear, invalid regex rollback,
field/numeric searches, Polars predicates and immediate editor closure.

Reference-driven title uses a shaded reflected red pixel heart above gold bitmap
lettering. Footer replaces unrelated blocks with heart + two-beat ECG pulse,
static idle/error/reduced motion, fixed label position. Source-less Escape modal
and CLI source bypass semantics remain unchanged.

Time dialog Alt-T opens reviewed timestamp enrichment prompt (timestamp_utc,
UTC RFC3339 microseconds, explicit input format, no guessed timezone/year/unit).
Added Python/native datetime formatting and timezone conversion allowlist entries.
Initial actual-native test exposed offset loss without Polars timezones feature;
feature enabled, offset correctness/rebuild validation pending. Existing event-time
basis still reads recognized raw fields; this prompt creates a derived column and
does not silently replace the time basis.


Timestamp feature validation now confirms actual Python-helper/native execution
normalizes 2026-09-05T15:30:00+02:00 to13:30:00.000000Z and malformed values to
null. Time-dialog Alt-T opens the editable reviewed prompt; it does not auto-run.

Broader runtime tests exposed a reintroduced terminal-admission race in stdin's
final watermark publication: finish_writer changed current.state toStopped, and
the watermark send published that state before writer join/runtime lease release.
Writer now sends final durable counters while retaining the currently published
lifecycle state. Supervisor alone publishes successful terminal state after join
and explicit fs2 unlock; explicit unlock also handles inherited open descriptions.
The complete35-test runtime suite and inherited-description unit regression pass.
Initial full real PTY also required modal-close handshakes and new numbered-stage
assertions/explicit Edit after successful Add; existing invariants remain tested.


Final preview026 checks:356 workspace Rust tests and24 Python helper tests passed;
workspace clippy with-Dwarnings and formatting/diff checks passed. Actual PTYs:
full real-source workflow, expanded search/clear/Time-dialog timestamp prompt,
iterative enrichment/restart, gzip, horizontal navigation, settings, title,
stdin, palette, demo and delight all passed. Runtime stress:10 parallel suites,
350 tests. One combined-load gzip PTY observed an empty plain-file reopen; focused
rerun plus10 complete repetitions (80 file opens/reopens) passed. No root cause
was established for that transient; the harness now retains failure artifacts.
No new live-provider request was issued for the timestamp prompt; proposal transport
is unchanged, and actual Python/native offset conversion is directly tested.

Search-reaffirm completions now retain the diagnostic of a different invalid draft.
A blank enrichment Add is a no-op with guidance, never an implicit chain removal.
Exact UI state tests cover both cases through the retained stage model and search
reaffirmation. Remaining explicit limits are recorded with preview026.

Published immutable preview026-search-extraction from d787c7b. Copied binary
passed the expanded search/rapid-clear/Time-dialog prompt PTY before latest was
atomically updated. SHA-256 a5e0ee1aed54a330f977c96e8a3d39353a1c91815bfa1d61642519c3a18a7a56.
All shared targets are released. Previous preview binaries are untouched.

## 2026-09-05 — task list, Luna default and heart-only pulse

Published canonical TODO.md, linked from README and implementation plan. It tracks
open bugs, unintegrated foundations, planned scope and published behavior; primary
updates it at every checkpoint and preview release.

New settings default to codex/gpt-5.6-luna, retaining persisted/environment choices.
Removed adjoining ECG line glyphs from the footer; only the fixed-width heart
alternates filled/outline in the existing double-beat rhythm. Idle/error and
reduced-motion presentation remain static.

Validation: 10 delight TestBackend tests, 11 settings tests, lvu/app clippy with
warnings denied, formatting and diff checks passed. Rebuilt demo/app; actual
delight and settings PTYs passed including resize, restart and terminal restoration.
No live-provider request was made for the default-only model change.

Published preview027-heart-luna from c56e3f6 after the copied binary passed the
settings PTY. Latest points to027; previous previews remain intact. TODO.md was
updated to distinguish this publication from preview026 feature completion.
Shared build targets are released.

## 2026-09-05 — extracted timestamp basis

Resumed the remaining-work queue. Time now offers explicit Alt-U for accepted
`timestamp_utc` enrichment strings; Alt-E remains raw recognition and Alt-P capture.
The basis is propagated through view/recipe persistence, rolling/absolute bounds,
selected-event anchoring and frozen snapshots. Snapshot parts add nullable
`_lvu_selected_time_unix_nanos`, preserving raw event recognition separately.
An active extracted-time filter prevents removing its required enrichment; failed
changes retain the applied chain and membership. Missing/invalid values do not
fall back to raw or capture timestamps.

Validation: lvu, memory, app and view tests passed (including new extracted-time
query, Parquet, recipe, persistence and UI regressions); relevant clippy passed
with warnings denied. Actual extracted-time PTY passed apply, arrivals, anchor,
restart, clear and terminal restoration. Initial harness quit needed an explicit
modal-close handshake; no production lifecycle change was needed. Existing raw
event-time diagnostic wording was retained after its regression caught a changed
prefix. A redundant feature-variant Polars rebuild was stopped; validation reused
the app's feature set on the shared target.

Search rapid-clear and gzip reopen PTYs also passed concurrently. The prior
intermittent empty reopen was not reproduced and remains open in TODO.md.

## 2026-09-05 — quoted field search

Search now accepts JSON-quoted field selectors (including spaces, punctuation,
escaped quotes, Unicode and empty field names within 64 bytes). A leading `\/`
requests literal slash text instead of regex syntax. JSON decoding uses serde;
Polars remains the matching engine. Existing search forms and debounce are retained.

Validation: lvu/app/query/view test suites and clippy with warnings denied passed.
Actual search PTY passed all forms, quoted fields, escaped leading slash, rapid
clear, invalid-regex rollback, timestamp prompt and terminal restoration.

Published immutable preview028 from aac0baf after the copied binary passed the
extracted-time PTY; latest now points to028. Four concurrent gzip suites passed
32 opens/reopens. Added case/open-number/prior-journal-size metadata and full ANSI
transcripts to future failure artifacts; did not claim the unreproduced bug fixed.
TODO.md and README reflect the published checkpoint.

## 2026-09-05 — live Luna timestamp acceptance

Actual timestamp assistance exposed missing proposal instructions: initial model
responses used an invalid envelope, a presentation separator, or statement-based
expression text. The bridge now includes the strict schema in the prompt, points
to the pinned Parquet reader, and explicitly requires single Polars expressions.
Parsing permits only one exact leading `---` presentation separator before the
otherwise strictly validated JSON; prose and multiple objects remain rejected.

Validation: bridge typecheck/build and all 43 tests passed. Actual Luna proposal
and native application passed, converting `2026-09-05 15:30:45 +0200` to
`2026-09-05T13:30:45.000000Z`. Retained proof:
`/tmp/lvu-timestamp-live-proof-4hjd63ua`; log:
`/tmp/lvu-timestamp-live-proof-validated.log`. Earlier failures remain archived
and were rejected without changing the accepted view. No model upgrade was used.

## 2026-09-05 — explicit capture stop/restart

Alt-S and Alt-R in logs/sidebar and corresponding palette actions stop/restart
the selected shared source without replacing its views. Work runs asynchronously
with bounded admission. Replacement registration waits for pending old-handle
query submissions to settle. Shutdown closes manager admission and settles owned
operations. Files use durable resume; commands require explicit restart. Stdin
restart is rejected before stopping its existing reader. Source state appears in
the sidebar; control results/errors take precedence in the footer until input.

Validation: lvu/app tests and relevant clippy passed. Actual PTY verifies file
stop/history/resume without duplicate bytes, command reaping and one startup per
request, accepted search across restart, and terminal restoration. The first PTY
expected an obsolete `search:on` label; changed its handshake to the actual
accepted draft and reran successfully. No HTTP/automatic-restart support claimed.

## 2026-09-05 — raw neighboring-record context

`o` and the palette open a read-only context dialog around the selected stable
record. Arrow/page/mouse scrolling inspect physical records from that source;
`g` returns to the anchor and Escape returns to the unchanged filtered view.
The raw provider handles bounded background paging; requests cannot cross source
boundaries, and the dialog avoids filling the cache with obscured viewport rows.
The anchor does not follow live selection changes.

Validation: lvu/app/live/view suites and clippy passed. Native test verifies hidden
neighbors, unchanged membership and source isolation. TestBackend covers small
geometry and fixed selection. Real PTY passes paging, hidden lines, live arrivals,
resize, filter preservation and restoration. Its initial status expectation was
updated to the dialog's own record total, since it covers the sidebar.

Published preview029 from af4fb55 after both copied-binary PTYs passed. SHA256:
`73b21cb11c40ef5d8afcc69000f638008f273c087896e017f9ba15498ec5b429`.
Latest points to029. Obsolete composite/review/text-search Cargo targets were
cleaned with Cargo; previews, captures and proof archives were preserved.

## 2026-09-05 — portable recipe export

Recipes now support Alt-E/export through the bounded memory worker. A request
captures the selected recipe/revision and destination; later selection or current
revision changes cannot substitute a different definition. Export validates and
syncs bounded TOML, then atomically links it to a new destination without replacing
existing files or symlinks. Source metadata remains included and is described in
the review dialog. Import/apply remain separate explicit actions. Recipe paths
support bounded single-line paste and Unicode.

Validation: lvu/app/memory tests and clippy passed. Memory regression verifies
old-revision export after current changes, fresh-workspace import, identity and
configuration preservation, unknown revision rejection and no overwrite. UI
regression verifies captured identity and stale-result fencing. Actual PTY passes
Unicode destination export, repeated export refusal, independent workspace import
and native search application, plus terminal restoration.

## 2026-09-05 — persistent bookmarks and notes

`b` toggles a stable record bookmark; `B` opens per-view bookmarks. Enter inspects
its raw context without changing filters, Alt-E edits a single-line note, and
Alt-D removes it. Notes commit on Enter; Escape discards the edit. Bookmarks and
notes persist with working views (not portable recipes); defaults load old state.
Bounds are 128 bookmarks/view and 1024 UTF-8 bytes/note. Source mismatch, duplicates
and oversized notes are rejected. User interactions fence delayed restoration.

The dialog uses the shared themed shell/cursor, scrolls in small terminals and
keeps mouse targets clear of the footer. Palette capacity is now 128 bounded
results so its complete static catalog remains browsable. Source-operation
notifications were generalized to transient action notices. Raw context now
reports out-of-index bookmarks and index failures instead of spinning indefinitely.

Validation: UI/app/memory/live/view suites and clippy passed. Actual bookmark PTY
passes Unicode notes, restart, hidden-record context, unchanged filter, removal
persistence and terminal restoration. UI tests cover edit/restore fences, note
bounds, small-terminal scrolling, mouse selection and anchored note editing.

## 2026-09-05 — bounded performance baseline and tmux

Added opt-in `bench:live`: real file capture, literal scan, advanced compilation
while appending, three time revisions, warm incremental append and cold viewport
paging with 32 rows/128 KiB cache and 128 KiB membership admission. All 61,000
records, 6,100 matches, sampled stable IDs and configured bounds passed. See
docs/performance.md for measurements and limits. The test's final expected count
was corrected from 6,000 to 6,100 when the additional warm append was added.

Context and bookmarks PTYs passed inside an isolated task-owned tmux server,
which was shut down afterward. No existing tmux sessions were touched. SSH and
long-duration throughput remain unvalidated.

## 2026-09-05 — bounded journal read-ahead

Journal pages now use 64 KiB buffered reads. A traced synthetic workload dropped
from 734,238 read calls to 2,715; comparable untraced wall times remained similar,
so no end-to-end speedup is claimed. Format, CRC validation and explicit page
offsets remain unchanged. Tests cover buffer boundaries, oversized/invalid-UTF-8
records, byte limits and later appends after EOF.

Validation: core/ingest/live/view/app suites, relevant clippy, formatting, the
opt-in performance workload and actual gzip/source-control PTYs passed. See
docs/performance.md for measurements. The intermittent empty reopen remains open.

Published preview030 from 076b9f8 after copied-binary bookmark and recipe-export
PTYs passed. Latest points to030; prior previews remain. SHA256:
`52169a87ee7a61c97850526399873a05df60fb57a006ca6c39e68e0e20a67cbd`.

## 2026-09-05 — immutable recipe history and explicit updates

Alt-H in Recipes lists up to 100 revisions newest first. Enter applies a reviewed
old revision through existing transactional native validation; Alt-E exports it.
Neither operation moves the recipe's current pointer. Alt-U reviews saving the
active view's accepted configuration as a new revision. Source, name and identities
remain attached to the recipe. The storage lock and SQLite transaction compare
the selected revision, rejecting stale writers before publication. Previous
revisions remain available after restart. History results retain dialog/request
fences; mode changes retire obsolete list loading state. Small terminals retain
the selected revision and footer. Palette exposes both operations.

Validation: UI/app/memory suites and clippy passed. Storage regression covers
immutable old documents, source/view identity, reopen and stale writer rejection.
UI regression covers delayed history, captured update identity and small-terminal
last-row visibility. Actual recipe-history PTY passes save/update, old revision
export/apply, unchanged current pointer, restart and terminal restoration. An
initial history test found the existing store page cap of 100; UI/API now use
that cap explicitly. Existing stale-loading test was updated for retired requests.
This source checkpoint is not yet published in preview030.

## 2026-09-06 — ordered recipe adaptation through the typed bridge

View proposals accept optional bounded inline enrichments. The Rust host checks
shape, field bounds and duplicate IDs; the native compiler checks semantics and
ordered dependencies. Omitted chains retain reviewed stages; empty chains propose
clearing them. The advanced filter and chain apply through the existing atomic
recipe path, preserving last-good state on failure. Requests include the actual
reviewed recipe definitions and reject contexts over 128 KiB before export.

Proposal review now supports PgUp/PgDn/Home with exact wrapped-line bounds using
the pinned Ratatui rendered-line-info feature; every stage source is inspectable.
UI tests cover chain submission, scrolling and atomic rollback. Host/schema tests
cover malformed bounds, extra fields and duplicate IDs. UI/app tests, clippy and
44 bridge tests/typecheck/build passed. Focused and full real-source PTYs pass
two dependent stages, native derived filtering, arrivals and restart. The first
focused invocation omitted fixed mise paths while isolating XDG and could not
start the compiler; the corrected harness and full suite passed.

Actual Luna proof passed in `/tmp/lvu-recipe-adaptation-live-proof-ypz3jl1j`: two
ordered stages (`level_copy`, then `error_flag`) and the dependent native filter.
Proposal/accepted screens, exact snapshot and transcript are retained. No provider
upgrade was needed. This source checkpoint is not yet published in preview030.

Demo and palette PTYs also passed. The demo initially exposed help text clipped
below its fixed-height body after earlier shortcut additions; help now uses up
to 24 rows, separates capture controls and puts mouse guidance near the top. The
final UI/app suites, clippy and formatting passed after that layout correction.

## 2026-09-06 — preview031 and authenticated SSH acceptance

Published preview031 from b4a3526 after copied-binary recipe-history and ordered
adaptation PTYs passed. SHA256:
`6dd31015ccadc8ccd91a6a886d24b022730887ea60a011e101d5d4d189aaa4ec`.
Context/bookmarks passed through an isolated key-only localhost OpenSSH daemon,
including resize and restoration. The reproducible `test:pty:ssh` uses strict
host-key/ownership checks and never edits user SSH configuration. It stops its
daemon and removes temporary private keys/authorization; proof logs remain in
`/home/venky/.lvu-ssh-pty-xjh1du6b`. Initial manual authorization under /tmp was
correctly rejected by StrictModes; an owned private home directory resolved it.
Remote latency/disconnect behavior is not claimed.

Freed obsolete build artifacts with Cargo: the old live-only target and cached
lvu-query/lvu-app package outputs in the shared target. No previews, captures or
proof archives were deleted. Latest points to031; remaining work stays active.

## 2026-09-06 — sustained capture/query acceptance

Added opt-in `bench:live:sustained`: 400 synthetic JSON records per 100 ms for
two minutes, 1% matching records, three full time-bound revisions and alternating
historical/tail paging. 481,000 records and 4,810 matches caught up in 119,992 ms;
5,587 returned viewport pages passed stable-ID checks. Maximum capture lag was
400 records; query lag reached 328 matches during full scans. Row cache remained
within 32 rows/128 KiB, membership within 128 KiB, with a 32 MiB derived-index cap.
Rust test-process peak RSS was 82,832 kB, excluding the helper. No indefinite or
arbitrary-volume performance guarantee is claimed. See docs/performance.md.

The full sustained run, final view/app clippy with warnings denied, formatting
and diff checks passed. The only post-run edit replaced equivalent modulo syntax
with the standard integer helper required by clippy.

## 2026-09-06 — explicit merged views and enrichment inspection

Added ordered source membership editing through `v`, Alt-M. Source edits publish
atomically with query membership; failed, queue-refused and superseded candidates
retain the prior complete view. Only already-open sources can be included. The
owning source remains required, and removing a bookmarked source is refused until
its bookmarks are explicitly removed. Cloning/persistence retain ordered sources.
Restoration defers merged views until all referenced sources are explicitly opened;
it never starts remembered commands. Workspace schema v3 marks the changed meaning
for older readers. Merged rows display a source column and order by explicit source
position then physical sequence, not event-time interleaving.

Native regressions cover undrained publication, failure, supersession, restart while
pending, snapshot source membership and raw-registration refusal. They also found
and fixed restart traversal stopping at an old-only page before reaching later new
records; the existing generation test now spans multiple old pages. UI tests cover
small source-list viewports and selection identity when hiding a deferred view.
The real merged-view PTY covers ordering, filtering, appends, cloning, shared
command startup, deferred restart restoration and terminal cleanup.

The enrichment screen now separates saved steps, a multiline draft, validation
status and input/output samples on sufficiently large terminals. Compact geometry
remains bounded. `str.replace` and `str.replace_all` are accepted and verified by
actual Python serialization and native Rust execution, including regex captures,
literal behavior, Unicode, nulls and partition equivalence. Both compiler layers
still reject unproven operations; this does not claim unrestricted Polars support.

Added visible-screen drag selection and Ctrl-C clipboard requests using Crossterm
OSC 52. Clicks dispatch on release so dragging cannot activate controls. Selection
retains the composited screen while capture continues; keyboard input, resize,
view/modal transitions clear it. Escape dismisses dialogs normally. Selection
storage is capped at 128 Ki cells; copy refuses over 64 KiB. Only visible text is
copied. Terminal clipboard support is required and the UI reports a request sent,
not an unverified clipboard acknowledgement. The real PTY verifies copying from a
dialog, dismissal, copying the log beneath, replacement execution, raw preservation
and terminal restoration. Unit tests cover wide/combining characters and frozen
screen content. Existing PTY clicks now send physical down/up pairs.

Proposal instructions prefer actual typed columns/sample values over regexing JSON
raw, without assuming an input timestamp field name. They require a reason for raw
fallback. Bridge schema/prompt tests pass. There is still no enforced per-source
sample count; that gap is explicitly tracked in TODO.md.

Validation: combined UI/app/query/memory/view tests passed (84 UI-state, 47 app,
11 settings, 29 memory, 22 query, 8 host, 26 view integration, plus unit/theme/palette
and documentation tests; two opt-in performance benchmarks were not rerun).
Python helper tests, bridge typecheck/tests/build, owned clippy with warnings denied,
formatting and diff checks passed. Full real-source, demo normal/panic, ordered
chain, copy and merged-view PTYs passed. Initial terminal failures were obsolete
label/visibility expectations after the layout change; raw-row assertions now run
after dismissing the larger editor rather than expecting hidden background rows.
The initial copy fixture lost tool paths under isolated XDG, causing helper timeout;
retaining explicit mise/uv paths fixed the fixture without changing timeout limits.


Controlled Luna acceptance also passed using a JSON `observed_at` field (not
`time`) plus an invalid value. The proposal used that column directly, without
raw regex extraction, and native application produced
`2026-09-05T13:30:45.000000Z` from `2026-09-05T15:30:45+02:00`.
Archive: `/tmp/lvu-structured-timestamp-proof-v33p8ax3`. No user log data was used.


Published immutable preview032 from `7ca1e2d`; copied-binary selection/replacement,
ordered-chain and merged-view PTYs passed before updating latest. The checksum and
capabilities are recorded in docs/previews.md and the local manifest. Prior preview
files remain unchanged.

## 2026-09-06 — current architecture and supported-feature documentation

Added docs/architecture.md as the current implementation map: actual crate/module
ownership, journal and view data flow, draft/publication fences, compiler boundaries,
XDG persistence, budgets, snapshot/agent validation and terminal responsibilities.
Reorganized README around supported preview032 features, examples, settings and
explicit limits. Updated AGENTS.md with concrete extension, UI, data preservation,
validation and immutable-preview guidance. Corrected stale single-stage/unwired
memory statements in the app README and the old model/theme/settings example.
The historical implementation plan now links to the current map; TODO includes
reported selection/redraw and compiler-support work without declaring it released.

Validation: checked local Markdown links and git diff whitespace; compared module
paths, mise tasks, settings fields/defaults and compiler limits against current
source. No runtime tests were needed for this documentation-only change. Existing
uncommitted implementation changes remain separate and unpublished.

## 2026-09-06 — user GIF converted to timed ANSI artwork

Installed Chafa 1.18.2 with a pinned Linux x86-64 download/checksum through mise.
Preserved the supplied source GIF under assets/startup/love-you-log-time and
converted all ten composited frames to padded SGR-only half-block ANSI at 80x22
and 120x40 cells. Both manifests retain the original 110 ms durations and infinite
1.1-second loop. Added reproducible conversion and standalone terminal playback
scripts/tasks, visual reconstructions, and asset-format/activity-indicator notes.
This is asset preparation, not startup or activity-indicator application wiring.

Validation: both manifests, every frame's dimensions/glyph/control sequences,
nonidentical animation content and original timings passed checks. Actual PTY
one-cycle playback emitted ten frames, exited successfully and restored termios,
cursor/wrap and alternate-screen modes at both sizes. The larger converted still
was visually inspected. No Rust/application build or preview publication occurred.

## 2026-09-06 — sharpen ANSI lettering without changing the heart

Added separate sharpened ANSI variants using a local contrast pass confined to the
lower lettering rows. Preview tasks now use these variants; originals remain.
Every frame's upper heart region is byte-for-byte unchanged, and all durations and
canvas dimensions are preserved. Both variants passed frame/control validation and
actual one-cycle PTY timing/restoration checks. Inspected the large reconstructed
still: edges are crisper, with the unavoidable small-resolution lettering limit
stated in asset notes. No application renderer or published binary changed.

## 2026-09-06 — consistent ANSI preview background

Fixed a real palette mismatch: converted padding used indexed ANSI black (theme
customizable), while Chafa pixels and sharpened lettering used RGB black. Both
padding and the preview's surrounding clear now use explicit RGB black. All forty
frame diffs were verified to change only padding background escape sequences;
art colors, canvas dimensions and timing are unchanged. Original and sharp variants
at both sizes passed actual PTY playback/restoration and frame validation again.

## 2026-09-06 — dialog selection and resize recovery source checkpoint

Selection now uses the rendered active modal/palette interior or originating pane.
Dragging clamps in either direction; copied text excludes dialog borders and
background content. TestBackend checks cover editor boundaries at 120x32 and
54x12, dismissal and tiny-terminal reset. Actual PTY checks decode OSC 52 and
compare complete interior text after dragging outside both opposing corners.

Every resize invalidates Ratatui's cached screen even when dimensions return to
their prior values. Ctrl-L requests the same recovery without closing an editor.
Draws use synchronized updates; line wrapping is disabled during the TUI and
restored with the other terminal modes. The new `test:pty:redraw` checks a stopped
app resized small/large/original, injected stale header cells, 100 long arriving
rows, footer visibility, Ctrl-L and terminal restoration. Injection demonstrates
cache recovery; it does not establish the user's exact screenshot root cause.

Validation: lvu/lvu-app/lvu-view tests passed; the added geometry test passed after
correcting its action variant. Final owned clippy with warnings denied passed
after simplifying an equivalent test inequality. Full real-source PTY, standalone
normal/panic PTY, copy and redraw PTYs passed. The initial redraw harness answered
only the first cursor query and caused a timeout; it now answers repeated queries
across read boundaries. Failure and corrected-run logs remain under
`/tmp/lvu-continuation-*`. No preview was published; parent artwork remains separate.

## 2026-09-06 — explicit snapshot sampling and exact proposal revisions

Snapshot manifests now request deterministic evenly spaced part-relative offsets,
including source endpoints, capped at 128 rows/source and 512 total. They prefer
accepted typed outputs and identify source-context fallback when no rows match.
Prompts require all part schemas, actual coverage and separately identified extra
reads. This is an inspection request, not an enforced provider I/O limit. Timestamp
guidance allows documented projection conflicts as a reason for raw extraction
and forbids substituting capture time. Bridge commit `faa4f9d` binds exact data and
definition revisions in both SDK output schema and inline schema; strict runtime
mismatch checks remain.

Native sampling units and actual Parquet offset/derived-field/stable-ID integration
passed with the full view/app/UI suites and clippy. Locked bridge install,
typecheck, 51 tests and build passed. The prior live attempt in
`/tmp/lvu-sampled-timestamp-proof-u0rd8rhx` failed safely on revision mismatch;
it remains recorded, not erased by later success.

After the schema fix one controlled Luna request succeeded in
`/tmp/lvu-sampled-timestamp-proof-rz98tai7`: exact revisions, reported 128-of-500
inspection, direct `observed_at` expression and native acceptance. The proof script
then sent navigation without waiting for the historical page and selected the
malformed first row, so its final UTC assertion failed (correct null displayed).
Without another provider request, reopening that same saved capture and waiting
for row readiness confirmed null for invalid input and
`2026-09-05T13:30:45.000000Z` for `+02:00` input, plus accepted-chain persistence
and terminal restoration. Both original failure and reopening proof are retained.
No preview was published.

## 2026-09-06 — integrate supplied startup animation and corner heartbeat

Embedded the large sharpened Chafa title (120x40), smaller 80x22 conversion and
compact ASCII/tiny fallback. A restricted parser converts checked-in SGR half-block
frames into cached Ratatui buffers; no runtime subprocess or raw escape output is
used. All ten original 110 ms frames and RGB black borders remain. Any key dismisses
and is consumed; Ctrl-C retains explicit quit, paste does not dismiss, and CLI
sources bypass the title. Reduced motion selects the first frame.

Converted the user's four-heart sprite sheet to a 14x7 corner indicator. Assigned
650/100/100/150 ms phases, reserved seven lower sidebar rows without changing log
capacity, and mapped near-black surround to the theme background. Active/pending
work animates; idle/reduced motion rest. Removed routine idle/working labels while
retaining error/progress reporting. Modal draw order and selector hitboxes respect
reserved geometry. Smaller studies were preserved outside the repository.

Validation: full lvu tests and clippy passed, app build/app+UI clippy passed, actual
startup any-key/CLI-bypass and full real-source PTYs passed. Color-enabled demo PTY
checks active sprite changes, return to rest, no routine labels, resize, reduced
motion, disable and restoration. Initial demo failure revealed missing Clear around
centered title and was fixed; an explicit background-filled TestBackend regression
covers it. A later animation assertion exposed inherited NO_COLOR=1 suppressing
terminal RGB, so visual PTYs now explicitly set NO_COLOR empty/COLORTERM truecolor.
No production override of user color preference was added. This topic does not
publish a new preview; broader compiler/dialog work continues under the delegated
GPT-6-Astra implementation supervisor.

## 2026-09-06 — broaden native expression semantics without method whitelists

Integrated compiler topic `254cbd4` as `4629ce2`. Python separates construction
from execution: pinned Expr-returning constructors/public transformation namespaces
are available, while eager frame/series construction, I/O, callbacks, plugins and
metadata tooling remain excluded. Datetime constructors are included. Rust retains
structural restrictions and explicit temporal formats, then uses Polars' own
row-separable/length-preserving IR metadata. Schema-free conversion validates
before publication (even on empty captures); real-schema lowering checks again
before execution. Existing size, identity and shape checks remain. Only the direct
polars-plan 0.55.2 dependency edge was added, with no dependency version upgrade.

Review corrected an initially overbroad Python construction surface and an
empty-capture validation gap. The final conversion uses Polars' unverified-schema
IR path rather than synthetic Null columns, retaining supported typed operations.
Native fixtures verify exact string/list/temporal values, dtypes, Unicode/nulls
and partition equivalence. Shift/reverse/forward-fill, aggregation and hidden unsafe
branches are rejected; unavailable native feature variants remain deserialization
errors. This does not claim every Polars operation or feature is enabled.

Implementer validation: Python 43, query 25, app 47, settings 11, host failures 8,
view integration 26 passed; two opt-in benchmarks skipped. Owned clippy/fmt/diff
checks passed. Supervisor reran Python 43 and view integration 28 (two new tests,
two benchmarks skipped), plus query/view/app clippy and formatting. Added tests
prove dependent strip/uppercase/slice stages retain values, IDs and live arrivals
after rejected neighbor edits, and an empty capture retains its accepted literal
stage after a rejected shift and subsequent arrival. Combined compiler/art app
build passed. Dialog redesign remains in progress; no preview published.

## 2026-09-06 — redraw recovery does not consume queued keyboard input

Combined artwork/compiler acceptance exposed a real cursor-report race: Ctrl-L
followed immediately by Escape entered Ratatui's synchronous cursor query and
failed with “cursor position could not be read.” Fullscreen recovery now uses
resize with the actual current terminal dimensions. This clears the viewport and
resets the cached screen even when dimensions are unchanged, without querying
the cursor; the next frame places its own cursor. Startup uses the same path.

The real PTY regression sends Ctrl-L and Escape together, verifies dialog close
and absence of a cursor query, and still checks resize round-trip invalidation,
live arrivals, footer visibility and terminal restoration. It passed, as did
startup any-key/CLI bypass and dialog/log selection PTYs, combined UI/app tests
and clippy. The broader real-source PTY reached the correct filtered result but
failed on obsolete “applied:” text after the dialog redesign; its assertions are
being updated under that topic. No preview was published.

## 2026-09-06 — grouped help and dialog semantics accepted in source

Integrated help176cddd as eefc881 and correction97e2e18 as b97c8f1. Help uses
aligned themed groups, adaptive columns and modal-owned scrolling. Search uses
a labeled editable row, muted examples, explicit state and retained accepted
filter; overflow status has a separate scroll hint. Source assistance remains
visible in the action footer. Discovery evidence/scan status and Storage errors
are scrollable separately from actions. Review restored omitted Time, Recipes
and Ask bindings. A final tiny-layout guard preserves editable rows and tells
the user to enlarge the terminal rather than silently losing shortcut actions.
The full modal interior remains the selection boundary. Dialog presentation
principles now live in docs/dialog-design.md.

Validation: combined UI/app build, 130 UI tests, 47 app tests, 11 app settings
tests, clippy with warnings denied, formatting and diff checks passed. Actual
color-enabled Help and Search PTYs check styles, cursor placement, scrolling,
background isolation, narrow errors and accepted-filter preservation. Combined
real-source PTY passed capture/discovery/completion/query/recipes/storage/agent
fixture/reopen paths; normal/panic demo, title, selection and redraw PTYs passed.
Focused recipe export/history, source control, bookmarks, raw context, merged
views and enrichment-chain PTYs passed after status/title assertion updates.
The final Search scroll-hint placement was followed by targeted status testing
and Search/Help/demo PTYs.

Initial integration PTY failures were obsolete applied/title/completion labels
and assertions against rows hidden by the taller modal. The tests now await
accepted state and close the overlay before inspecting those rows. One initial
resize test continued before the application's resized frame; the final test
waits for changed/restored cursor geometry before typing. This is test sequencing,
not a claimed production fix for every resize/input interleaving. The earlier
Ctrl-L cursor-query race is separately fixed and documented above.

No preview was published from this worktree. Parent will build the final main
revision so embedded Python/bridge paths use main, test a copied immutable
preview033 candidate, and coordinate the latest switch.


## 2026-09-06 — publish preview033 from main

Integrated reviewed compiler, redraw and corrected Help/dialog checkpoints. Built
app at `d969c4c9615fe74b4f19725bb7a1e8d889794bd3` on main and copied it before acceptance. SHA256 `8fcdd5638b59cb8d8a6e9e0db189c5852e7e88ba3ab25a9c93ef169289a35703`.
Copied-app Search, startup/CLI bypass, clipboard boundaries, redraw/queued Escape,
ordered enrichment and full real-source PTYs passed. Built main demo passed color
Help, heartbeat and normal/panic restoration. Bridge rebuild passed.

Initial Help invocation mistakenly supplied the app binary to the demo harness;
`--demo` was rejected. Correct demo invocation passed. Heartbeat acceptance then
revealed a fixture race: DEMO text remains visible behind Search, so the old wait
could snapshot the still-open dialog as the resting heart. Added an explicit
Search-close handshake; heartbeat and normal/panic demo checks passed. No production
change or timeout increase was needed. Original logs remain in /tmp/lvu-preview033-*.

Manifest records the main runtime checkout dependency. Previous binaries remain;
latest now selects033. Remaining command work stays under supervisor ownership.

## 2026-09-06 — durable command-attempt prerequisites

Integrated runner e9ce2c3 as 9c9eb45 and SQLite store e23226a as 5cd82a5. The
runner reserves IDs before handing payload to its writer, uses a store-owned
reservation token for final outcomes and fails closed on an ambiguous reservation
acknowledgement. Cancellation/deadline is rechecked after reservation. Reserved
IDs cannot be retried implicitly even when no output could be durably finalized.
Store methods own their transaction/lock timeouts. Compatibility run_batch callers
retain the in-memory ledger; no command UI/app execution was connected.

SQLite schema v4 adds atomic, capacity-checked reservations keyed by view/stage/
command/preceding-definition revision, exact-set token-owned completion and
immutable terminal results. Reads preserve caller order and reject cumulative
payloads over 1 MiB. Full u64 sequences, typed JSON and Ready diagnostics survive
reopen; raw capture payloads are not duplicated. Source/view/recipe migration
tests pass. Older previews reject a v4 database; this source change is unpublished.

Supervisor integration adds three actual SQLite/subprocess tests for lost
reservation acknowledgements with zero delivery and no retry after reopen, typed
Ready results across mixed batches/reordered responses, and final-persistence
failure after delivery with no repeated delivery. A first fixture attempted to
rewrite an input field and was correctly rejected; it now writes a new field.
The combined test then exposed hash-set iteration randomizing input delivery.
Serialization now preserves caller order while retaining ID-based response joins.
Completion encoding checks its aggregate budget incrementally.

Runner 15 protocol tests, memory 36 tests and all three combined integration tests
passed. This uses injected acknowledgement/finalization failures plus database
reopen; it is not a claim of complete app integration or power-loss testing.
App tests (47 plus 11 settings), command/memory/app clippy with warnings denied,
formatting and diff checks also passed. The integration test adds only a workspace
dev-dependency edge from command-enrich to memory; no dependency versions changed.
Preview033 remains the published binary.

## 2026-09-06 — command input/editor and presentation support in progress

Integrated corrected frozen reader f6d74d6 as e5c8459 and command editor 33de908
as 7b56f9e. The reader captures accepted source generations, high-watermarks,
membership and native definitions, replays bounded batches and preserves original
record bytes. Review added rejection of native-stage replay diagnostics before
exposing command input and cancellation on empty/completed input. Five focused
reader tests and view/query/app clippy passed in the implementer worktree.

Command editor review corrected stale confirmation after draft edits, unsaved
draft execution, unbounded request growth and save acknowledgements after closing
the dialog. It retains separate save/prepare/review/run steps and an app-owned
durable publication reference. UI 136 tests and clippy passed; this remains a UI
boundary until the controller and actual app terminal flow are accepted.

Supervisor support adds bounded frozen snapshot preparation, independently stored
command definitions/publication references, typed result restoration after DB
reopen and read-only Details decoration. Native fields, membership and raw context
remain unchanged; new records show pending only with a configured command. Atomic
presentation admission caps each result set at 1024 records/1 MiB and total command
presentation at 8 MiB. Control characters are escaped for Details display while
the stored typed values remain exact. Metadata-only attempt existence reads avoid
loading result payloads for admission.

Support checks passed: app 50 tests plus 11 settings, memory 36, runner 15 plus
three SQLite/subprocess integration tests. The first shared-target run reused an
older memory artifact; refreshing the changed local memory source timestamps
forced the required rebuild. One new test used the wrong store method name and
was corrected before the passing run. Unused controller support currently produces
dead-code warnings; final app clippy and actual app PTYs remain integration gates.
No user database or preview was changed. Opening this source's schema-v4 database
with preview033 or older is unsupported; retain this note for separate publication.

## 2026-09-06 — command controller acceptance found integration failures

Execution module 8cdf700 was integrated as d2a6028 after a supervisor regression
found that replies without batch completion could otherwise be reused as success
on another Run. Global batch failure now finalizes new token-owned records as
Failed. Nine real SQLite/subprocess module tests and app clippy passed in the
implementer harness. Prior Ready records remain immutable; valid completed
batches can retain independent event successes. Root narrowed admission existence
checks to metadata and corrected cancellation wording to avoid claiming that no
delivery occurred.

Portable Ctrl-S/Ctrl-R/Alt-N command controls and their optional enhanced aliases
are integrated, including empty final argument preservation and correct cursor
placement on trailing newlines. Root fixed stale save acknowledgement tokens
leaking bounded request capacity. Full UI 137 tests and clippy pass.

The first actual composed app PTY failed after saving a command with blank working
directory: controller normalization changed the accepted definition but not the
visible draft, so Prepare refused it as unsaved. Review additionally found that
definition saves cleared previous publication references/results, stale Ready
reviews could keep the controller busy, filesystem checks ran on the UI tick,
restore errors could write stderr or disappear, and persistence queue refusal
could lose controller acknowledgement tracking. The provisional controller is
not integrated. Its implementer is correcting these together with state-machine
tests and bounded shutdown-save handling. The failure log is retained at
/tmp/lvu-command-first-app-pty.log. No new preview or user database execution.

## 2026-09-06 — composed command flow and remaining save boundary

Corrected controller 91e4ee2 is integrated as 2ca7c19; Details scrolling ae53f49
as 08f7ee5. The original blank-cwd and publication-retention failures are corrected.
Root combined app/query/view/memory/runner suites passed 201 tests, with two opt-in
benchmarks skipped (/tmp/lvu-command-combined-tests.log). The root app build passed.

Actual PTY reached typed native input delivery, explicit new-arrival-only rerun,
restart without delivery, restored Ready fields and malformed-protocol rollback.
Details scroll exposes late command fields; selecting a new arrival correctly
resets its scroll. Harness corrections add explicit modal-close acknowledgement,
scroll Pending into view, and replace 4096 unnecessary backspaces with the exact
known argument count plus field readiness. Those failures were test interaction
assumptions, not evidence of lost command results. Logs remain at
/tmp/lvu-command-combined-pty.log and -pty2.log through -pty4.log. The last run
correctly displayed the protocol failure and previous Ready fields but omitted
an explicit Error status label; the UI correction is pending.

Review found a real publication race: cancellation after execution but during
metadata persistence could still publish, while suppressing acknowledgement alone
would leave a new durable reference. The correction will define accepted save
dispatch as the commit boundary, validate freshness/cancellation before it, and
show an immutable Saving results state with Close rather than Cancel afterward.
No compensating write is claimed to eliminate a crash window. Restore tracking
now includes definition revision as well as publication reference (d29a495), so a
same-reference revision change cannot strand restoration. Final combined tests,
PTY, clippy and publication remain pending. No user database was opened.

## 2026-09-06 — terminal command enrichment accepted in source

The optional terminal command stage now passes actual application acceptance.
Root integrated the SavingResults UI/controller boundary, explicit Error labels,
Pending warning styling and narrow Details controls. A final root regression fixes
matching stale run completions retaining request capacity after view revision
changes; ten consecutive stale completions preserve last-good state and allow
another review.

The first final PTY passed the SQLite writer-lock SavingResults/close test, then
found an unnecessary restore after editing a command: revision-aware observation
reloaded the unchanged old publication and briefly refused Review as busy. The
final correction fences restoration by owning view and immutable publication
reference, independently of the later command definition. Actual reference changes
still prune/fence stale restores. This supersedes the revision-aware restore design
noted above. Evidence: /tmp/lvu-command-final-pty.log, then the complete passing
/tmp/lvu-command-final-pty2.log after the correction.

The passing command PTY exercises native typed inputs, Save without delivery,
review cancellation without delivery, explicit confirmed execution, stable IDs,
wrapped wide/combining Unicode Details, pending arrivals without automatic work,
new-ID-only rerun, all-Ready reuse, closing while SQLite blocks an admitted result
save, restart without delivery, malformed-protocol rollback, truecolor input/accent
roles, narrow controls, normal terminal restoration and Save immediately followed
by quit. The final fixture database confirms the last environment edit and old
publication reference survived; no user database was used.

Final validation: UI 141 tests; app 67 plus 11 settings; memory 36; runner 15 plus
three durable-store tests; query 38; view 33 with two opt-in benchmarks skipped.
Combined six-package all-target clippy passed with warnings denied, followed by
app clippy again after the restoration correction. Formatting/diff checks passed.
Logs: /tmp/lvu-command-combined-tests.log,
/tmp/lvu-command-final-integration-tests.log,
/tmp/lvu-command-restoration-final-tests.log,
/tmp/lvu-command-final-clippy.log and /tmp/lvu-command-final-clippy2.log.
Full real-source PTY and copy/selection regression passed, recorded in
/tmp/lvu-command-regression-real-pty.log and /tmp/lvu-command-regression-copy-pty.log.
The final app build is /tmp/lvu-discovery-ui-target/debug/lvu-app.

This is accepted source, not a new preview. Latest remains033-dialogs-and-heartbeat.
Publication must rebuild from main, test its copied candidate and explicitly note
schema-v4 compatibility. Multiple/interleaved command stages, command-dependent
native predicates and recipe persistence remain separate unchecked work.


## 2026-09-06 — simultaneous resize and Escape readiness

Copied command acceptance exposed a terminal input bug while enlarging its dialog
and immediately pressing Escape. A minimal independent probe reproduced it;
FIONREAD reported one byte still queued after the timeout. Crossterm 0.29's active
mio backend can return a resize from a readiness batch before consuming its TTY
edge, then clear that batch on the next poll. Enabled its existing use-dev-tty
level-triggered backend alongside osc52, with no version change; the lockfile adds
only the existing filedescriptor dependency edge.

The 24-cycle PTY queues resize and Escape while stopped, then resumes without a
rescue key or a redraw handshake before Escape. Identical checks fail on the old
binary and pass on the new backend. An initial test wrongly assumed FOLLOW starts
at column1 despite the corner heart; the final check verifies the bottom footer
text instead. UI/app clippy passes. The initial strace attachment yielded no trace
and is not evidence; live queued-byte inspection and old/new PTYs are evidence.
Proofs: /tmp/lvu-resize-fionread-8j1q5ddu and /tmp/lvu-preview034-*-regression2.log.
Publication remains gated on the corrected copied candidate's full acceptance.


## 2026-09-06 — transparent heart pixels use the background channel

The user's Terminal-theme screenshot exposed white horizontal bands around the
corner heart. Near-black source pixels were assigned Color::Reset in either
channel, but Reset foreground resolves to the terminal's foreground, often white.
The renderer now composes upper/lower pixels explicitly: two transparent halves
become a space; one transparent half uses the background channel and the matching
upper/lower block orientation. Both opaque colors, including the white pulse, are
retained. No theme color assumption, asset resize or cadence change was made.

Eleven delight tests pass, including Terminal/dark/light margin and foreground
checks; app/UI clippy passes. Actual terminal regression checks the transparent
margin contains spaces. Publication awaits rebuilt candidate acceptance.


## 2026-09-06 — preview034 publication

Published copied main-built source `7bbe24f236ce026262f32954d65766f7992fcf07`, SHA256 `b68c3534064ff8863e6076a92b3e8110c49c76005e9c1ba63016eacf77f1d07e`.
After accepted command integration, added heartbeat pending-work tracking through
closed-dialog result commits. Corrected simultaneous resize/input readiness and
Terminal-theme transparency from reproduced failures. User then requested half
size: Chafa 7x4 conversion retains original timing and returns three sidebar rows.

Final 11 delight tests, corner geometry and UI/app clippy passed. Final copied
command, 24-cycle resize/Escape and clipboard PTYs passed, as did the final demo
heartbeat/transparency PTY. Corrected backend before artwork-only final changes
also passed command/stdin/title/copy/fullreal and normal/panic demo. Logs retain
all failures and corrected runs under /tmp/lvu-preview034-* and /tmp/lvu-half-heart-*.

Manifest records runtime main checkout and schema-v4 incompatibility. No user DB
was used in acceptance. Latest now points to034; previous binaries remain intact.


## 2026-09-06 — publish quiet status as preview035

Removed the redundant main-footer shortcut inventory, retaining only ? help.
Updated the command/redraw test workspace markers. 101 UI-state tests, UI clippy,
copied command and24-cycle resize/Escape PTYs pass. A focused copied terminal
check confirms the short hint, absent inventory, functioning Help and restoration.
Published main source `35cac018cabacccc6b7d73b737d466d51d9e7715`; SHA256 `c511f1e9605b9cf13afd4505c47bac3394a6b037e16e32dca634ba2a6aa9144d`.
Latest selects035; prior immutable previews, captures and proof archives remain.


## 2026-09-06 — preview036 layout and Search hierarchy

User screenshots showed the footer starting at fixed18 columns inside the22-column
sidebar and the heart centered in that smaller area. Status now aligns to actual
log.x;5x3 artwork centers across the complete sidebar. The footer remains outside
sidebar ownership. Search drops its redundant input label, places applied state
before examples, and uses readable base foreground instead of DarkGray.

Full UI tests, clippy, centered-art/footer geometry assertions and actual demo
heartbeat passed. Copied Search/color/status,24-cycle resize/Escape and clipboard
PTYs passed. Targeted cargo clean-plvu-view removed obsolete test/build artifacts
to recover space, preserving Polars caches, captures, proofs and all previews.
Published `fe0db13a8db96d907d7a351ae1684b6bfc658358`, SHA256 `ed27f410c829f9b5379e5152e7b89c4e76dd73d02de0ca6c7b4598e8b16532c4`; latest selects036.

## 2026-09-06 — heart inside the full-height sidebar

Moved the existing 5x3 heartbeat to the bottom-left interior of the Sources/views
pane. Its border now continues around the reserved artwork rows; list rendering
and view hitboxes stop above them. Status remains aligned to the log pane. No
startup alignment, sprite size, colors or cadence change is included.

Full lvu tests (including 101 UI-state tests), all-target clippy, formatting and
actual demo heartbeat PTY pass. Tests cover full borders, artwork/list separation,
modal coverage, transparent margins, arrival animation, resize and restoration.
Logs: /tmp/lvu-corner-tests.log, /tmp/lvu-corner-clippy.log,
/tmp/lvu-corner-pty.log. Source validated; preview036 remains unchanged pending
main app integration and copied-candidate acceptance.

## 2026-09-06 — timestamp assistance latency follow-up

Parent's read-only inspection of session
`eeb62a5b-22e9-4b30-866d-b8b92a198674` reported medium reasoning and seven
shell/Python calls navigating an approximately 40,000-line manifest, schemas
across 40 listed Parquet parts, samples and conversion checks. The activity
artifact is `/tmp/lvu-timestamp-session.log`; the similarly named `.json` file is
CLI text, not a structured timeline. These observations identify inspection work,
not a measured attribution of all elapsed latency.

Recorded bounded prepared schemas/sample inputs and visible session activity as
follow-up work under snapshot sampling/investigation ownership. No optimization,
provider retry or build was performed for this diagnosis. Existing sampling
coverage remains a request rather than an enforced provider I/O cap.


## 2026-09-06 — product-focused README

Reorganized the README into a feature punchlist, source quick start, longer
feature overview, platforms and concise caveats/next directions. Differentiators
are tied to implemented behavior: capture reuse, incremental Rust/Polars work,
additive enrichment, preserved raw data/last-good views, reusable workflows and
optional bring-your-own agent assistance. Removed shortcut inventories and preview
history from the product narrative; moved contributor commands, environment/data
reference and validation conventions to docs/development.md. Removed stale claims
that command enrichment was unintegrated and Datetime support was pending.

Relative documentation links and referenced mise tasks checked; git diff --check
passed. Documentation-only change: no build, provider call or preview publication.


## 2026-09-06 — README tone correction

Removed the slogan and promotional headings after user feedback. Retained the
requested feature punchlist, quick start, detailed features, platforms, limitations
and planned work, using direct descriptions. Development reference remains
separate. Relative links and diff checks pass; no runtime or preview changes.
