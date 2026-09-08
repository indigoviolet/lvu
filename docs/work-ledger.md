# Implementation work ledger

Validation evidence by assignment, owned by the primary agent. Newest entries
are at the end. Entries before 2026-09-07 (the empty-repository baseline,
milestones M1 to M7, previews 001 to 043) are in
[`work-ledger-archive.md`](work-ledger-archive.md), moved verbatim on
2026-09-08.

## 2026-09-07 — field correlation across sources completed and integrated

Reused three reviewed commits from the earlier attempt, cherry-picked in order onto
main and reconciled with the rebuilt Fields dialog, the owned-Storage component and
the `shell.cursors` move: `ab6d9c3` (lossless native exact-field boundary in
lvu-query), `bb21018` (fenced UI correlation requests) and `95a5611` (persisted
accepted exact-field view constraints). Conflicts were all both-sides additions
except the Fields renderer, where main's empty-state/read-only-footer version was
kept and only the `r Correlate across sources` action and the pending state were
added. `add_accepted_view` from `95a5611` was dropped: the integration installs a
correlated view through the existing `add_view` + `restore_persistent_view` path
the Clone flow already uses, and keeping a second 120-line view constructor with
no caller would rot.

New work: `FieldCorrelation` in lvu-core (per-source field mapping, bounded to 32
sources, refusing an empty mapping); the lvu-view lookup seam
(`submit_correlation_lookup` / `take_correlation_lookups`) running one bounded
cancellable scan on its own thread with explicit incompleteness for the field-name
sample; exact-field projection and execution inside the existing query worker with
unmapped sources contributing nothing; a class-M `Correlate across sources` mapping
dialog built on `dialog_layout` with a §8.3 anchored field popup and §10 Escape
layering; and controller wiring that orders the correlated view's sources the way
the user opened them so records keep explicit source position then sequence.

Two behaviours were found only by running the actual app. The correlated view's
sources were first ordered by source identity, not by open order, so the merge
interleaved wrongly; and restoring `exact_field` as accepted made the restore
request's base snapshot disagree with the adapter's applied constraints, which
refused the query and left the view raw. The base snapshot is now fenced against
the *previous* correlation while the accepted one is installed immediately, so a
save taken before the first scan completes cannot drop it.

Validation: `cargo fmt`, `cargo test --workspace`, `cargo clippy --workspace
--all-targets -D warnings` clean. New tests: two lvu-view integration tests (two
sources with `request_id`/`req`, correct records in explicit order, unmapped source
excluded, unreachable record and cancelled lookup), five app tests (no implicit
mapping, rejected/cancelled mapping leaving the origin view intact, empty mapping
refused and rendered, correlation surviving a rejected later edit, correlation
naming a foreign source refused) and a new `test:pty:correlation` suite.

Rebased onto `2031599` after main adopted Fields onto the dialog anatomy. Correlate
is now a third action button in that dialog's action row (`FieldPickerControl::
Correlate`) beside Pin and Color, `r` still reaches it, and the pending lookup
replaces the action row with a Pending message row and withdraws the row hitboxes
so click and paint agree. Nothing else in the rebuilt dialog was touched.

PTY matrix 49/50 with the new suite included. The one failure, `test_lvu_real_pty.py`
(`timed out waiting for screen containing 'command stdout'`), reproduces identically
on unmodified `2031599` under the same 4-worker matrix and passes serially; baseline
measured in this worktree before rebasing. Earlier `test_source_ai_review_pty.py`
and `test_lvu_real_pty.py` failures were only this worktree lacking `bridge/dist`;
after `mise run build:bridge` both pass.

## 2026-09-08 — checklist rows shipped before preview 047

Moved verbatim from `TODO.md` when the checklist was restructured around preview 052; these were Done at or before preview 046. Order is the order they held in the checklist.

| Status | Request |
| --- | --- |
| **Done** | Fix the five long-standing PTY failures, including a JSON fixture that never had a build target. — Earlier entry: Reconcile the PTY matrix against the new dialog vocabulary; 37 of 42 suites pass. |
| **Done** | Restructure the enrichment dialog into two layers: a step list with Add/Edit/Remove, and a focused step editor showing input, expression and output, saving back to the list. |
| **Done** | Fix the Source 🧠 proposal being unreviewable at small terminal sizes: launch/cwd/restart/env/why must be reachable before Start reviewed. |
| **Done** | Fix silent data loss: with a legacy `.lvu-captures` dir in the cwd, the capture root flips between runs and abandons sources, workspace, filters and enrichment. |
| **Done** | Restore a visible Apply action in the grouping dialog: the universal-shortcut cleanup removed the `Enter Apply` hint without replacing it, so the dialog has no actions region. |
| **Done** | Give the grouping `[ Apply ]` action a mouse hitbox. |
| **Done** | Investigate blank restored filtered views, including cases where the query reports matches but no rows appear. |
| **Done** | Complete file paths automatically while typing; remove Complete path and Open buttons; Enter opens the selected file or enters a directory. |
| **Done** | Restore Up/Down selection in discovery and file suggestions directly from the search/input field; Enter opens the selected result. |
| **Done** | Support installation through Homebrew. |
| **Done** | Support installation through mise, including relocatable helper/runtime resources. |
| **Done** | Support selecting arbitrary timestamp fields, beyond the accepted `timestamp_utc` enrichment. |
| **Done** | Improve automatic timestamp/epoch recognition without requiring AI. |
| **Done** | Add HTTP/reconnect sources and explicit command restart policies. |
| **Done** | Add ownership-aware capture/investigation deletion, retention and cache-pressure handling. |
| **Done** | Debounce search and keep the applied filter synchronized when the text is quickly cleared. |
| **Done** | Search plain text in `raw` or another field, including quoted/Unicode field names. |
| **Done** | Search `/regex/` in `raw` or another field, with literal-leading-slash escape support. |
| **Done** | Support more complicated `pl.col(...)` filter expressions. |
| **Done** | Broaden Polars expression support, including string replacement and `pl.Datetime`, while rejecting unsafe cross-record behavior. |
| **Done** | Keep the last valid filter/enrichment active when a new draft fails. |
| **Done** | Preserve ordered enrichment steps and allow later steps to use earlier fields. |
| **Done** | Replace repeated “native” labels and Alt-A/E/R-only enrichment controls with visible actions and an explained external-command option. |
| **Done** | Support Ctrl-A, Ctrl-E, Ctrl-K and arrow editing in shared text inputs. |
| **Done** | Separate field-expression completion from explicitly labeled static sampled values. |
| **Done** | Put timestamp recognition inside the Time dialog. |
| **Done** | Prefer usable structured timestamp fields; do not assume the input is named `time` or regex-parse JSON unnecessarily. |
| **Done** | Derive the `timestamp_utc` output and validate its format before applying it. |
| **Done** | Prefill Time bounds with editable date, time and timezone segments. |
| **Done** | Put Start fields on one row and End fields on another where space permits. |
| **Done** | Provide Time basis/window dropdowns, timezone choices and visible Apply/Clear/AI actions. |
| **Done** | Preserve custom Time drafts, offsets and precision across reopen/restart. |
| **Done** | Use Luna by default for new AI configurations while preserving saved model choices. |
| **Done** | Validate AI proposals against JSON schemas, exact revisions and actual expression/data checks. |
| **Done** | Send compiler/output instructions with AI requests; schema validity alone does not make a proposal valid. |
| **Done** | Put typed schema, sampled values and coverage directly in short-request prompts; avoid making the agent assemble samples from Parquets. |
| **Done** | Bound prompt bytes and sample counts; report omissions and coverage. |
| **Done** | Group helper sessions under lvu and archive completed helpers; keep investigations resumable. |
| **Done** | Retain AI session activity for inspection. |
| **Done** | Deduplicate schemas in full-export manifests and pack compatible batches into fewer Parquet files. |
| **Done** | Provide a bounded Polars inspection helper for full snapshots. |
| **Done** | Allow text selection and Ctrl-C copy inside dialogs and behind them after dismissal. |
| **Done** | Keep drag selection inside the active dialog/pane boundary. |
| **Done** | Repair resize/live-arrival redraw corruption and the simultaneous resize/Esc input-loss bug. |
| **Done** | Remove PgUp, PgDn, Home and End bindings; retain focused arrows and scrolling. |
| **Done** | Let the Discovery diagnostics pane receive focus and scroll. |
| **Done** | Make Discovery diagnostics readable with and without focus. |
| **Done** | Remove the repeated Search heading; place Applied above examples/help. |
| **Done** | Remove redundant basic-navigation, Mouse and non-action Review entries from Help. |
| **Done** | Color JSON keys consistently and automatically without changing the underlying text. |
| **Done** | Use data-driven key/value colors beyond a five-color palette, with dark/light affordances. |
| **Done** | Show an explanatory Fields dialog when `i` has no fields or selected record. |
| **Done** | Convert the supplied animated artwork through Chafa and integrate the large title. |
| **Done** | Sharpen title lettering, preserve the heart and remove inconsistent background edges. |
| **Done** | Dismiss the initial screen with any key; skip it for CLI sources. |
| **Done** | Use the supplied heart sprite for activity, with no extra triangle or idle/working text. |
| **Done** | Shrink the heart and place it at the bottom-left inside the full-height left pane. |
| **Done** | Align the main footer with the log pane and keep only `? help` as its shortcut hint. |
| **Done** | Write architecture and agent guidelines; keep development notes separate from the README. |
| **Done** | Make the README factual: feature summary, quick start, detailed features, platforms and caveats, without slogans or a development log. |
| **Done** | Push the repository to GitHub. Later local commits are not automatically pushed. |
| **Done** | Preserve raw bytes and stable record identities across file/command/stdin capture and resume. |
| **Done** | Read static gzip archives with reopen and corruption diagnostics. |
| **Done** | Discover sources and open files, commands and stdin from the CLI or app. |
| **Done** | Provide independent named views, merged source views and restored working state. |
| **Done** | Provide Details, pinned fields, multiline grouping and horizontal event scrolling. |
| **Done** | Provide bookmarks, notes and bounded neighboring-record context. |
| **Done** | Provide explicit source stop/restart without silently restarting remembered commands. |
| **Done** | Run one reviewed external-command enrichment after expression steps, with durable attempts and last-good results. |
| **Done** | Save, revise, export and adapt reusable recipes. |
| **Done** | Provide memory/index limits and reviewed cleanup of unused derived indexes. |
| **Done** | Replace the compaction-loop supervisor with a fresh agent while preserving implementers and worktrees. |
| **Done** | Keep one readable checklist with explicit statuses; do not mark unreleased feedback fixes as done. |


## 2026-09-08 — the filter was not waiting, it was computing

The soak's report of the worst defect in the product — a 620k-record filter at
10.75 s p50, linear in record count, indifferent to selectivity, "0.18 s of CPU
across 10.57 s" — named a wait: a poll interval, a timer-paced channel, a round
trip per page. Sampling the app's own `/proc/<pid>/stat` around a single filter
on that same 620k capture says the opposite. Before: 11.12–11.76 s of wall clock
and 15.54–21.60 s of process CPU, a ratio of 1.40–1.84. The scan was saturating
more than a core for the whole eleven seconds. `strace` for `futex`, `poll`,
`nanosleep` and `fsync` was not needed to rule a wait out; the CPU accounting
already did, and a per-phase timer inside `run_query` accounted for the wall
clock without an unexplained gap.

Where it went, per scanned record: 3.42 µs reading and decoding journal frames,
1.75 µs building the Polars frame, 12.0 µs in the batch execution, 0.83 µs in
membership and time bookkeeping — 18.2 µs, or 55,000 records a second.

Four causes, largest first. The `dev` profile built every dependency
unoptimised, and lvu has no release build at all: previews, the PTY matrix and
the soak all run the `dev` binary, so an unoptimised Polars, `regex`, `memchr`
and `crc32fast` ran under every record the product ever scanned. Query pages were
256 records, so a journal round trip, a frame, a plan and an engine run were
amortised over almost nothing. Each scanned row formatted its source UUID into a
fresh string for the identity column and then a second one for the per-batch
identity backstop, which inserted it into a `BTreeSet`. And every scanned record
was hashed into a map of basis timestamps and a set of matched sequences.

After: the same filter over the same capture takes 0.71–0.81 s with 0.74–0.88 s
of CPU, 705k–838k records per CPU-second. That is 15x on the clock and 21x on the
CPU-normalised measure, and the after run was taken at load average 13 against
the before run's 2.6. Per phase, at 620k records: 0.33 µs read, 0.19 µs frame,
0.36 µs execute, 0.06 µs membership; 0.95 µs total, 1.05M records a second. A
3 GB source extrapolates to about fifteen seconds per full filter rather than
about eight minutes. `docs/performance.md` carries both tables.

`mise run soak` over the same 620k source, on the final rebase and on a box at
load average 5 to 8: p50 10.75 s to **0.727 s**, p99 11.05 s to **0.824 s** over
24 timed filters, at 1.10 seconds of app CPU per second of query wall clock.
That clears the soak's own one-second query budget, which it no longer reports
as a failure. Three cycles completed; resident memory went 27.1 to 53.3 MiB with
a 61.6 MiB peak and nothing climbing, capture held at 108.1 MiB and the derived
index at 23.7 MiB, worst shutdown 0.135 s, slowest input-loop iteration 0.037 s.
Capture of the 64 MB source settled in 89 s here against 300 to 460 s on the
same binary and input under contention, which is the capture row's evidence more
than this one's.

The p99 over 24 samples is the maximum, so it is the number most exposed to what
else the machine is doing, and it behaved accordingly: the same code reported
1.629 s and 2.97 s for that maximum on busier runs while the p50 barely moved
(0.754 s and 0.651 s). Driving all six of the soak's literals by hand at load 26
to 30 costs 0.53 s to 0.79 s each and 1.0M to 1.3M records per CPU-second, the
match-nearly-everything `seq` filter included at 0.69 s, so even then no single
filter is slow. Every run still stopped in a fourth cycle waiting for
`Fields · record`, which is the harness-hardening row, not this one.

One soak attempt in between produced no query timings at all: the build volume
had been filled to 1.1 GB free by other work, capture stalled at 14,848 records,
and the run timed out after 900 s waiting for first rows — the W22
blank-viewport row, arriving as a full disk rather than as a query result.
`mise run janitor` reclaimed 35.9 GB.

Dependencies now build at `opt-level = 2` and workspace crates at
`opt-level = 1`; debug info and incremental compilation stay off, as this host
requires. This is a root `Cargo.toml` change and every existing target directory
will rebuild its dependencies once and keep the superseded artifacts until
`mise run janitor` runs, which is worth flagging to anyone else building here.
Rebuilding `lvu`, `lvu-view` and `lvu-app` after an edit measured 38 s.

New coverage asserts throughput rather than a wall-clock number, because the
defect is a scan that waits and that is a property of the pipeline, not of the
machine: `crates/lvu-view/tests/scan_throughput.rs` captures 150k records, runs
one literal filter and requires a CPU-to-wall ratio of at least 0.5 and at least
100,000 records per CPU-second (measured: 1.61 and 535,000; the old scan managed
30,000–40,000). `mise run soak` now records the app's CPU per query beside the
wall time so the same misreading cannot recur, and a cycle that fails part way
still prints the samples it collected instead of losing them to the traceback.

Capture is a different defect and keeps its TODO row. Two of the app's tokio
workers sit in `jbd2_log_wait_commit` at about 5% of one core: it waits on
`Journal::sync_data`, which runs every 512 records, on a volume several agents
write to. Its rate varied by more than an order of magnitude on identical
binary and input as that contention changed. The journal being 1.7x the source
is arithmetic, not a defect: 16 header bytes plus 58 fixed body bytes over the
generator's ~109-byte records is 1.68x.

Validation: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets
-D warnings` clean; `cargo test --workspace` passed on six of eight runs. Both
failures were `source_membership_changes_publish_atomically_and_preserve_failed_
or_superseded_views` failing with `Journal(AlreadyOpen)` while restarting a
stopped source, under load average 19 and 20 with other agents building, and
each passed on rerun and in isolation. That is the restart race `8e306c5`
reproduced independently on main at the same call site; this change touches no
locking, stop or lease code.

PTY matrix 55/55 at two workers before rebasing, then 55/55, 56/56, 57/57,
58/58, 59/59 and 60/60 twice as main added suites under it, the last on the
final rebase onto `eed97ca` at load average 5.4. That rebase also merges main's
journal-lock ownership fix into the same `journal.rs` functions this changed,
and its `restart_race.rs` reproducer passes here. Each green run was taken on a quiet
box on purpose. At load 13 and above the same tree reports 53 to 57 of the
suites and fails a different set each time on three- to nine-second internal
budgets — `test_lvu_real_pty.py` on `command stdout` and on `┌ Enrichment `,
`test_default_actions_pty.py`, `test_empty_event_fields_pty.py`,
`test_capture_root_pty.py`, `test_lvu_pty.py`, `test_shared_dialog_controls_pty.py`,
and `test_shared_list_dialogs_pty.py` exiting 8.10 s against an 8.00 s limit —
and every one of them passes when run alone. One run was killed outright by a
concurrent janitor reaping PTY scratch. Two early attempts failed
`test_source_ai_review_pty.py` because this worktree lacked `bridge/dist`;
`mise run build:bridge` fixed that permanently.
