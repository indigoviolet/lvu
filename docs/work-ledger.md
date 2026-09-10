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

## 2026-09-08 — the underlined letter was not a key

Reported against the Fields dialog, whose row draws `[ Pin ] [ Filter ]
[ Exclude ] [ Color ] [ Fold ] [ Correlate ]` with `F`, `x`, `C`, `d`, `r`
underlined: "`x`, `F`, `d` don't do anything here", then "alt+f alt+x don't
work at least in my terminal". Both are true, for two different reasons, and
the second one invalidates the rule §8.10 had.

**Why each key did nothing.** The underlines came from `&`-marked labels
through `dialog_controls::mnemonic`, but the keys behind them were written by
hand in each dialog's `key()`. In `components/fields.rs` that was
`Char('c')`, `Char('r')` and `Char('o')` bare — arms that predate mnemonics —
and `Char('p'|'f'|'x'|'d') if alt`. So `c` worked bare because of a legacy
arm, and `f`, `x` and `d` were Alt-only. A letter the layer does not bind
returns `Outcome::Ignored`, and `apply_outcome` drops it: `key_to_action`
returns `Action::None` for `Focus::Layer`, so nothing falls through to the
base screen either. `d` did not toggle Details and `f` did not toggle follow;
all three reached nothing at all. `F` is `Char('F')` with SHIFT and matched no
arm.

**Why Alt did not work either.** Measured on xterm 400 under Xvfb by reading
the bytes the emulator writes to the pty. With xterm's default
`metaSendsEscape: false` every Alt+letter is a Latin-1 character, not a chord:
Alt-f is `c3 a6` (U+00E6 `æ`), Alt-x `c3 b8` (`ø`), Alt-d `c3 a4` (`ä`),
Alt-Shift-F `c3 86` (`Æ`), and likewise `á â ã å ç è é í î ð ò ó ô õ` for
a b c e g h i m n p r s t u. With `metaSendsEscape: true` the same keys send
`1b 66`, `1b 78`, `1b 64`, `1b 46`. crossterm reports the ESC form as
`KeyModifiers::ALT` and the 8-bit form as an unmodified `Char('æ')`. lvu
enables no keyboard-enhancement protocol, so that is the whole encoding. Alt
was never a mechanism that could be relied on.

**The rule now** (`docs/dialog-system.md` §8.10): the bare underlined letter
presses the button whenever no text field has focus, because with nothing to
type into the letter is not text; when a text field has focus only Alt+letter
does; Alt+letter always works; the match is case-insensitive. Inside a dialog
a letter that is also a base-screen key resolves to the dialog's mnemonic —
the layer owns its keys (§7.5, §10) and no base binding reaches it. The 8-bit
meta form is deliberately not decoded back into a chord, because `æ`, `ø`, `ä`
and the rest are letters a user may need to type and the only place the
decoding would help is a focused text field, which is exactly where they must
stay text.

**One implementation.** `dialog_controls::mnemonic_press` answers "which button
does this key press", and `App::dispatch_raw` — the single function every
layer's raw input passes through — asks it before handing the key to the
component, using the layer's own `Component::action_labels` and `text_focus`.
Thirteen keymaps stopped spelling mnemonics out: Fields lost its `c`/`r` and
Alt-`p`/`f`/`x`/`d` arms, Storage its `r`/`c`, Enrichment its four, Time
Alt-C/Alt-T, View Alt-B/C/R/S, Bookmarks Alt-E, External command Alt-S/R/M/N,
Recipes Alt-A/S/U/H. Every dialog whose row varies with state now computes it
in one function `render` and the shell both call, as §8.9 already requires of
the default action, so a drawn underline and a live key cannot disagree.

**What kept its old spelling, and why.** View's Alt-D (Clone) and Alt-M
(Sources), Bookmarks' Alt-D (Remove) and External command's Ctrl-S/Ctrl-R are
not letters of their labels, so they are not mnemonics; they stay as the
unlisted aliases §8.10 allows. Fields keeps bare `o` for Raw context: `Raw
c&ontext` is only in the row when the record has no fields, so with fields
present `o` is an alias for an operation with no button, and two tests already
depended on it. Recipes keeps bare `x` for Reject because it is the one layer
that reports `text_focus` whenever its `More ▾` menu is closed, which
suppresses bare mnemonics there; `Reject` gained `&Reject` so it has a
mnemonic when focus does leave the field. Bookmarks' `Edit note` and `Remove`
gained the underlines their existing Alt chords never showed.

**Palette and Help agree.** A layer whose row is reachable bare prints the
bare letter in the palette's shortcut column (Fields `f x c d r`, Storage
`c`, Time `c`, Enrichment `a e r`, Bookmarks `e`); a layer that opens with a
text field focused prints the Alt chord that works from there (View
`Alt-B/C/R/S`, External command `Alt-S/R/M`). Help's Conventions now reads
`Underlined letter — Press that button` followed by `Alt + letter — The same,
from inside a text field, where the letter is text`.

**Help no longer prints command-line options.** W25's resume-sources commit
(`1cfa343`) added a `STARTING LVU` section listing `lvu`, `lvu --resume`,
`lvu --fresh` and `FILE / -c CMD`. Help documents the keys and behaviour of
the app that is running; `lvu --help` and the README own the command line, and
a flag printed in Help is a key nobody can press. §8.10's Help row says so and
both the Rust and PTY help tests assert it.

**A diagnostic.** `lvu --keys` (`crates/lvu/src/keys.rs`, ~90 lines, no TUI,
no workspace, raw mode restored on every path) prints the decoded key, the
bytes and the modifiers for every key pressed until Ctrl-C. Driven through a
real xterm it reproduces the table above from lvu's own decoding: `Char('æ')
/ c3 a6 / none` on defaults, `Char('f') / 1b 66 / ALT` with
`metaSendsEscape: true`. It is the difference between "the terminal never sent
a chord" and "lvu did not bind it".

**What the tests found.** `crates/lvu/tests/mnemonics.rs` walks every layer,
takes its live action row through `App::top_layer_action_labels`, and compares
the rendered screen after the bare letter with the screen after Alt+letter.
Two real facts came out of it. Settings and View publish `text_focus` from
their last render (§1), so a test that opens a layer and presses a key with no
frame in between asks a question the real loop never asks — the test draws
between them, as the shell does. And View opens with the caret already in its
Name field, so its `b`/`c`/`r`/`s` are text there and become keys only once
Tab moves focus off; that is why its palette rows print the Alt chord and not
the letter.


**Rebased onto `14ad91c` (preview054)**, which had moved twice underneath this.
Two conflicts, both real: W28 gave Fields a `Fol&d`/`Unfol&d` toggle, folded
into `action_buttons` so the label and its `d` still come from one function;
and W14 landed Ask and Investigation as layers, added to both new shell
accessors and to the audit. Neither agent dialog marks a letter — `Submit`,
`Apply`, `Cancel request`, `Send`, `Resume`, `Start`, `Open`, `New snapshot` —
so they contribute no mnemonic, but they are in the inventory now, so one
cannot be added there without a live key.

Validation: `cargo fmt --all --check` and `cargo clippy --workspace
--all-targets -D warnings` clean on the rebased tree. `cargo build -p lvu-app
-p lvu` clean.

`cargo test --workspace` passes, with one flake that is not this change's:
`lvu-core`'s `gzip_stop_interrupts_fingerprint_before_decoding`. `git diff main
-- crates/lvu-core` is empty on this branch, and the same test binary gave
seven passes and one failure over eight consecutive runs with nothing changed
between them, at load 30-45 from other agents building.

PTY matrix 61/63 at two workers in 811 s on the rebased tree, taken at load 33
rising to 55 — the matrix's own ceiling is 16. `test_fields_mnemonics_pty.py`,
the new suite, passed, as did `test_help_pty.py`, which failed a first run for
a real reason and is fixed: at 72x16 the CONVENTIONS rows sit below the first
screen and above the last, so the suite now takes a capture of its own for
them. The two remaining failures are the two largest suites and both are load
flakes, shown rather than assumed. `test_lvu_pty.py` fails on `late arrival
continuing through active search` against a three-second budget; run three
times back to back at load 49 with the same binary it failed once and passed
twice. `test_lvu_real_pty.py` failed at three different points across three
runs — `command stdout`, `named-views`, `search:"beta"` — and passed the whole
story end to end in between, which no deterministic break does. Neither
suite's failing step touches anything this change edits: the Search editor's
action row is `["Apply"]`, which marks no letter, so its `action_labels` is
empty and no key is ever diverted from it.

One correction worth recording about the rebase itself. Rebasing a commit
written against an older main applied without conflict markers but resolved
five files in favour of this branch that it never meant to touch, silently
reverting main's `COMMAND` fix: `crates/lvu-app/src/main.rs` lost the
`COMMAND` const, `help_text()` and both of its tests, and
`crates/lvu/src/terminal.rs`, `docs/distribution.md`,
`docs/release-runbook.md` and `tests/soak/soak.py` went back to spelling the
binary `lvu-app`. The signal was an `lvu-app` test failure that looked like
one more load flake and was not. All five were restored from main and the two
`--keys` additions re-applied on top, leaving `crates/lvu-app` at +7 lines.
The check is `git diff main --stat` after a rebase: files the branch has no
business editing mean it has reverted someone.

Four separate runs of clippy or the workspace tests died on
`extern location for <crate> does not exist` or a linker `cannot open
lib*.rlib`, each time naming a file that had existed minutes earlier. The
cause is `scripts/janitor.py`: `prune_target` keeps the newest `--keep` files
per (stem, extension) and unlinks the rest across every `*-target` on the
volume, with no check for a build holding them. Any agent running `mise run
janitor` therefore breaks every other agent's in-flight cargo invocation, and
one of its own preflight runs crashed with `FileNotFoundError` on a file it
raced itself to delete. Retrying is the only workaround from here; the fix
belongs to whoever owns that script.

## 2026-09-08 — one Filter dialog: Search and Advanced as tabs (W29)

The user asked how `/` and Advanced filtering interact and whether they should
share a dialog with tabs. They were two class-S dialogs on two keys with two
applied states, both applicable at once and combined with AND, and neither
told the user the other was in force. Decision: keep both, keep AND, and make
one **Filter** dialog with a `Search │ Advanced` segmented control
(`docs/dialog-system.md` §12.1). Per the user's amendment `p` is retired: `/`
is the one key, opening on Search; the Advanced tab is reached with Alt-A, a
click, Tab then Left/Right, the palette row `Filter › Advanced`, or an
accepted 🧠 filter proposal. The title names every applied constraint from the
same `ViewState.{search,advanced}.applied` fields W28's view summary reads.
`[ Apply ]` is the filled default and `[ Clear ]` (Alt-C) drops the active
tab's constraint alone; Clear on a tab with nothing applied only empties the
draft, so it never forks a derived view off All events for an empty filter.

Shell: `LayerId::Search`/`Advanced` → `LayerId::Filter`, one `Layers.filter`
slot whose `purpose` is the active tab; `Open::Search`/`Open::Advanced` remain
and carry the tab (`Component::Open = Option<QueryPurpose>`). The segmented
control learned `&` mnemonics. Palette rows renamed `Filter › Search` (`/`) and
`Filter › Advanced` (no chord); Help lists `/` once; README updated. Unchanged
by test: fork-on-All-events, invalid-draft, the debounced search race
(`test_search_race_pty`), the Advanced completion popup on Tab, Alt-Enter.

PTY suites that pressed `p` now open via `/` and Alt-A through one shared
helper (`open_advanced_filter` in `test_lvu_pty.py`); markers naming the old
titles moved to `┌ Filter` and each tab's help sentence; nothing else in them
changed. Two Rust tests were tripped by the new layout and fixed at the
heuristic, not the assertion: the palette category-column test found the word
`Filter` in a row *name*, and the search-footer colour test read the first `E`
on the help row, which is now the sidebar's `Errors only` behind a shorter
dialog. Two Tab-focus tests now expect the tab control as the first Tab stop
on Search, the diagnostics pane second.

With W27's shell-side resolver on main (`712bb60`), the editor's own
Alt-S/Alt-A/Alt-C branch is gone: the Filter slot lists `Apply`, `&Clear`,
`&Search`, `&Advanced` through `Component::action_labels` and presses them in
`press_action`, as View lists its mode segments, so bare `s`/`a`/`c` switch or
clear once Tab has moved the keys off the field and the mnemonic audit's
inventory names the three letters.

Validation, on the tree rebased onto `73ff4c6` (W14's Ask/Investigation
components and W19's time display landed underneath; the rebase was clean and
one expectation main added for the Ask apply path now names `LayerId::Filter`):
`cargo fmt --check` and `cargo clippy --workspace --all-targets -D warnings`
clean. `cargo test --workspace`: 1052 passed; 13 failed in one run at load
45, all in crates this change does not touch (`lvu-command-enrich` protocol
×11, `lvu-ingest` runtime, `lvu-view` scan-throughput) and all passing when
rerun; an earlier run failed the `lvu-app` source-restart test with a closed
journal, the restart race the previous entries record, which then passed.
PTY matrix at two workers: 59/61 before the rebase (the agent-review suite
needed `mise run install:bridge && mise run build:bridge` in this worktree;
the real suite failed the Esc race above), 59/62 and then 57/62 after it, at
load 32–52 throughout with two other agents' matrices and builds on the box.
Every failing suite (settings, correlation, dialog system, shared list
dialogs, the real suite, the demo suite, layered dismissal, empty event
fields) passed when run alone, and the seven suites this change edits passed
alone repeatedly. A single 62/62 run was not obtained: the box never went
quiet, and three of the matrix failures were the 8 s exit budget and a 3 s
dialog wait at load 35–47, the same shape the previous entry records. The
target directory also lost dependency artifacts three times mid-build
(another session's janitor or the sccache restart); each rebuild recovered.

Gate on the tree rebased onto `1f45a57` (W27's bare mnemonics
underneath, before main was rewritten without them): fmt and clippy clean; `cargo test --workspace` 1092 passed with
one failure, `lvu-core` acquisition's `gzip_stop_interrupts_fingerprint_before_
decoding` (a stop signal losing its race to the decoder at load 28, untouched
code), which passed three times in a row alone; PTY matrix **63/63** at two
workers in 365 s at load 18–28.

Final gate on the tree transplanted onto `origin/main` at `e745d43` (the local
checkout's `main` had carried W27's mnemonics and then dropped them; the
branch now sits on what is pushed): fmt and clippy clean; `cargo test
--workspace` 984 passed with two `lvu-app` unit failures in one run (the
source-restart race again, and `agent::tests::critical_lifecycle_queue_
overflow_faults_host_instead_of_dropping_ack`), both passing three times in a
row alone; PTY matrix **62/62** at two workers in 273 s at load 12–25.

Gate on `design/filter-tabs-mnemonics`, the same commit carried onto
`origin/main` at `6f88666` with W27's resolver underneath (the Filter slot
exposes `Apply`, `&Clear`, `&Search`, `&Advanced` through `action_labels` /
`press_action`; the editor's own Alt branch is gone): `cargo build` clean;
fmt clean; `cargo test --workspace` 1127 passed, 0 failed; PTY matrix
**63/65** at two workers in 881 s at load 24–47, the two failures the 8 s exit
budget (`test_dialog_system_pty`, `test_shared_list_dialogs_pty`), both passing
alone afterwards at load 43–45. `cargo clippy --workspace --all-targets -D
warnings` stops on main's own `crates/lvu-query/tests/binary_column_scan.rs:21`
(`manual_is_multiple_of`, from `6f88666`, not this branch); `clippy -p lvu -p
lvu-app --all-targets -D warnings` is clean. The target was `cargo clean`ed and
rebuilt between the two gates (6.7 → 1.6 GB).


## 2026-09-08 — the Fields pane counts the whole view

The Value pane described the selected field over the first 2,048 records of the
view. With a filter now scanning at about a million records a second, counting
every record it holds is affordable, so the pane shows both: the sample
instantly, the whole view when it arrives.

Four layers, each its own commit because each has rules worth testing before the
next depends on them. `lvu-query::column_stats` folds one column's statistics
across the batches of a scan with Polars aggregations, memory following the
number of distinct values rather than records. `lvu-view` runs the pass over the
view's membership, bounded and cancellable, superseded when the selection moves,
using the chunked membership's ascending sequences to decide what is in the view
by binary search rather than by evaluating the filter twice. The shell holds one
question at a time and drops answers to superseded ones. The pane prefers the
whole-view counts and says which it is showing.

Two divisions of labour carried the design, and both are commented where they
bite. The engine counts and the app names — the app decides what a field is from
the record's own bytes and hands that over as a verdict the engine counts rows
against — which is what stops the sampled and whole-view figures disagreeing
about what a number is. It turned out to be enforced by the crate graph rather
than by discipline: `lvu` has no Polars dependency, so it could not hand over an
expression even if it wanted to. And the ordering that `min`/`max` mean is the
one thing the engine cannot express for itself, so the caller casts and the
comparison happens in the type the app already chose; without it "1000" sorts
before "9" and the pane reports a range no reader would accept.

The heading had to change with the numbers. The sample stops counting distinct
values at 4,096 and reports a floor; `n_unique` over the membership is exact. So
the same field goes from "4,096+ values" to "7 values", and a reader who cannot
see why would reasonably conclude one of them is wrong. `first 2,048 records`
becomes `first 2,048 records · counting the rest` and then `all 619,272
records`.

Cost: 620,000 records in 3.14 s, 3,000,000 in 13.72 s — linear, about 200,000
records a second, and over the second the design allows for. The design is what
makes that tolerable rather than the number, and it is worth being explicit
that this was accepted rather than fixed: the sample renders immediately and
stays for the whole pass, the pass dies when the selection moves, and a failure
leaves the sample showing. Nothing waits for it. It is also five times slower
per record than a filter scan over the same data, because the pass builds the
full typed projection for every batch to read one column; projecting only the
column asked for is the next thing worth doing and is left as a row rather than
smuggled in here.

Validation: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets
-D warnings` and `cargo test --workspace --locked` clean. 22 new tests: 8 at the
engine layer including identical figures whether records arrive in one batch or
three, 5 at the view layer including statistics scoped to a filtered view's
membership and nothing else, 7 on the shell's question-at-a-time contract, 2 on
what the pane says. One PTY suite, `test:pty:whole-view-stats`, which asserts
that the pane never claims more than it has rather than that the sample is
visible first — depending on losing that race would fail on a fast machine.


## 2026-09-08 — a statistics pass that reads one field

The whole-view pass cost 2.95 s at 620,000 records, five times a filter scan
over the same data. It is now 0.90 s, and 3M went 13.73 s to 4.06 s.

Worth recording how it was found, because two plausible answers were wrong.
The first guess was the projection alone; replacing it with a single-column
build barely moved the end-to-end number, and the fixture had grown in the same
change, so the comparison flattered it. The second guess was that Polars could
read the field out of the raw text faster than parsing it here; measured, that
is *slower* on two-field records and only marginally faster on ten-field ones,
and it would silently undercount logfmt and unstructured records that the
sampled figures read correctly — the same class of mistake as the nested-field
bug the PTY suite caught last time. Timing the pass's phases settled it:
projection 1.46 s, aggregation 1.15 s, journal read 0.21 s.

Both were real. The pass called the canonical projection — a column per field
the record carries, ten of metadata, three copies of the record's text — to read
one column, and converted every nested object back to text on the way. And it
aggregated once per journal page, running several Polars plans 151 times over
620k records. Projecting one field took projection to 0.57 s; buffering 65,536
records per aggregation step took aggregation to 0.09 s, which is the same
per-batch fixed cost that the filter scan's page size turned out to be.

What remains is the parse, and it is not removable: 2.17 µs of the 2.68 the
targeted projection costs on ten-field records. Reading a field out of JSON
costs what it costs.

`crates/lvu-view/tests/stats_throughput.rs` guards it in the style of
`scan_throughput`: records per CPU-second, not a duration, because throughput
per unit of CPU is a property of the pipeline and how fast a machine supplies
that CPU is not. The floor was set from an A/B on that test rather than picked —
320,000 records per CPU-second with the targeted projection, 113,000 with the
canonical one, floor at 200,000. It also waits for the derived index to finish
before measuring, because process CPU cannot tell the index worker's threads
from the pass's and a measurement taken across it reports one as the other.

Validation: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets
-D warnings` and `cargo test --workspace --locked` clean.


## 2026-09-08 — resume the interrupted coordinator with Sol workers

Read the recent Paseo history of coordinator `952b2ae8-3e5f-4a2e-be15-d3c6638152e8`
and the twelve recent worker sessions after the Claude weekly limit stopped
progress. Coordinator `526c36a1-fd82-4e0c-b86d-829949160668` now owns integration
on `integration/sol-continuation-20260908`, starting at `12d8341`; main and
published previews have not been moved. The replacements use the configured
GPT-5.6-Sol-Implementer profile (`codex/gpt-5.6-sol`, full-access), carry recent
history and explicit remaining assignments, and reuse each assigned workspace.
These are new sessions with transferred context, not native provider-session
forks. Original sessions, branches, stashes and captured data are preserved.

| Workstream | Previous session | Sol continuation |
| --- | --- | --- |
| W13 | `08d3bb98-8114-492d-a5da-f5e47798c47a` | `9b721ee5-0979-4d4f-af76-ca4a0c2a8d77` |
| W14 | `22f9ca5c-ff32-4c2b-98ff-28d5b6ea8899` | `c44c7092-a4ae-4312-a87e-6f6b57340248` |
| W18 | `78a2be2a-0806-470f-8b9e-2938dd619800` | `e5fd8069-ae86-4a6c-9493-d0a93ad07e6c` |
| W19 | `de547a5a-0e6b-424a-bbb8-f2cd8a7652e2` | `d5d43aef-367f-4015-80c4-4ea37961c73d` |
| W21 | `a665f1e8-572e-4d8f-b00b-eebf52a6ff76` | `41be6f30-df27-47e2-af96-8d0fbbef2304` |
| W22 | `6c58a889-10cb-468b-80b7-1065387006f5` | `2eaba630-8a0e-41fb-bdb7-b9770d6d23cc` |
| W23 | `5d12f33c-51af-4bed-8a9d-7cc96a9b2240` | `c4b5def8-961c-4cff-925b-5423194d1100` |
| W24 | `0e085206-4a08-4307-b0da-cda3ee019d28` | `b3f728d4-2bc1-4bef-ab9e-b55ae5888418` |
| W25 | `30e1a709-3458-42ed-80ee-15b9d50c0f63` | `a13b5d00-4c6d-4f7d-81f6-552042a33897` |
| W26 | `0a435950-9541-411f-acd6-6cccabfe7e18` | `f26af803-9573-44fc-bb40-e4ac2272cade` |
| W27 | `795cb630-22d9-4674-aad9-73604631ea1e` | `d2bca6de-74fa-45fa-bcdc-b3539dbcaff5` |
| W28 | `a17ead60-9373-4f42-8555-ebaba70207ce` | `6a205f20-e422-4203-a986-5b899edf8d4b` |

W31 (`95afce55-0bbd-4cdf-8567-515836f53333`) owns the unanswered ANSI presentation
request in a new isolated worktree. W14 also owns the source-discovery prompt
bug; W22 will assess traceback folding after its existing status/FOLLOW fixes.
Completed old topics remain excluded; W26 only reconciles the already-published
release. W13's remaining changes are the journal-fixture cleanup, screen-text
matrix coverage and reproducer sweep; W23 owns freshness-script reconciliation.

Costly validation is serialized with `flock` on
`/mnt/HC_Volume_106796581/lvu-build/sol-validation.lock`, with a separate target
per worktree, disk preflight and scratch on the large volume. The lock file is
not deleted. Each worker reports validation against its resulting commit;
recovery alone does not establish integration.

Recovered baseline evidence: the predecessor's completed
`bjoiznnmx.output` names `12d8341`, 1,176 workspace tests passed / 0 failed,
clippy and fmt clean, and PTY matrix 66/66 in 182 seconds at two workers.
This is recovered evidence, not a fresh run by the new coordinator. The earlier
projection-pick failure was a TODO/performance-document conflict; the explicit
retry succeeded. The projection is local and validated, still absent from
published preview 057.

W26's read-only reconciliation verifies release `v0.1.1` was published at
18:20:10 UTC, tag target `3fc6bdda9082d776cb2eaab60dc3d813d2258317`, workflow
run `34258813789` successful, all four archives and SHA256SUMS present, and tap
commit `3a5a8885afb0e5b0734046fd617a93d286c47fff`. Its predecessor recorded
clean credential-free Linux Homebrew and mise installs; those installs were
not repeated during recovery. Linux arm64 and Darwin archives were executed
in CI; interactive/human acceptance remains x86_64 Linux only. No release was
republished. Documentation is corrected to distinguish those coverage levels.


## 2026-09-08 — freshness follows each binary's dependencies (W23)

Integrated worker revision `1147ffa` on the Sol integration branch. The
preflight uses cached Cargo metadata to compare each binary with its non-dev
dependency closure, retaining package/root manifests, Cargo.lock and build
scripts while excluding unrelated crates and test/bench targets. Metadata
cache invalidation reads workspace and local-path manifests without walking
previews, captures or dependency installations. W13's test-source exclusion
is included here; its older freshness commit must not also be picked.

Primary validation: `mise exec -- python scripts/test_build_freshness.py`
passed 3 tests, including actual preflight refusal/passing outcomes with
controlled timestamps and content-based metadata invalidation. The live
preflight passed against the primary target's existing binaries.
`git diff --check` passed. No Rust build or full PTY matrix was run for this
script-only integration; combined integration validation is pending.


## 2026-09-08 — Sol's ambient NO_COLOR caused six PTY failures

W13 reported 61/67 and W14 60/66; W21's focused rerun lost the same six
suites at load 3.0–3.4. Three waited for the Advanced Filter tab despite its
body being visible; Details, the demo and sixteen-colour suites failed style
assertions. Untouched product files did not establish attribution.

Primary inspected the transcripts and environment: `NO_COLOR=1` is inherited
by these Codex sessions. The harness merged it into every child unless a suite
explicitly overrode it. All foreground/background cells were `default`, and
the active-tab helper reads bold to distinguish Search from Advanced.
A paired check against the same existing primary lvu-app binary passed
`test_details_colouring_pty.py` with `env -u NO_COLOR`, then failed immediately
with inherited `NO_COLOR=1`, reporting the all-default cells. No build changed
between those runs.

`isolated_launch` now sets the default test value to empty and retains any
explicit per-test override, including no-colour tests. The harness self-test
checks demo and real launch paths and verifies the parent environment is
unchanged. Both self-tests pass. Full `mise run test:pty:matrix` is queued
under the shared lock; no clean matrix result is claimed yet. Workers were
notified to use the corrected harness or explicitly unset ambient NO_COLOR
for their next PTY run; passing Rust/TypeScript checks need no repetition
solely for that environment correction.

W13's narrow journal-fixture cleanup and explicit matrix collection of
`test_screen_text.py` are integrated as `5035616` and `15c991b`. The worker
reported its targeted journal test, formatting, clippy and builds passing.
Its workspace run stopped at a command-enrichment protocol failure and its
solo retry passed; that is not a completed workspace pass. The janitor
reproducer sweep remains unintegrated pending ownership/protected-data guards
and committed fixture tests.


## 2026-09-08 — reproducer cleanup requires explicit ownership (W13)

Integrated corrected worker revision `15de41a`, replacing the rejected broad
name-and-age sweep. `.lvu-test-reproducer` marks a disposable test tree;
`lvu-*`/`w<number>-*` and age only select candidates. Ownership, root symlink,
fresh-descendant, live cwd/cmdline and root/nested capture/proof/preview guards
leave those trees intact. Accounting tolerates disappearing files and reports
removal only after successful deletion with the root absent. AGENTS.md routes
scratch to the large volume and explains the exceptional /tmp marker policy.

Primary ran `mise exec -- python scripts/test_janitor.py`: six synthetic tests
passed, including dry-run preservation, successful synthetic deletion and
failed/disappearing deletion accounting. `git diff --check` passed. No real
/tmp sweep was performed for this revision, and the predecessor's 2.40 GB
removal is not validation of the stricter implementation. No Rust/TypeScript
checks were repeated for this Python-only correction.


## 2026-09-08 — multiline readability extends beyond tracebacks

The user clarified that the pending multiline work should handle any messy
multiline output, not only Python tracebacks. W22's queued design now covers
wrapped messages, pretty-printed payloads, diagnostics, stack traces and
stray continuation lines through the existing Grouping/Folding controls.
Recognition must remain bounded, source/stream-local and reversible, retaining
all original bytes and identities and showing incomplete output immediately.
ANSI cleanup remains W31's presentation responsibility. This is a scope
clarification and design assignment; generic automatic grouping is not yet
implemented or validated.


## 2026-09-08 — Correlation component integrated (W21)

Integrated `f70bef6` as `e8a6b7e` on the Sol continuation branch. The worker
rebased onto `2992750`; primary verified range-diff equality with validated
`30c06b8`. Correlation owns lookup/mapping state and rendering; the shell
routes runtime results through the component boundary. Remaining legacy
handoffs are non-dialog actions, including raw-context/bookmark jumps,
recipe adaptation and applying proposals. This is local, not published.

Worker evidence on identical content: app/TUI builds, 12 Correlation component
tests, workspace tests, clippy with warnings denied, and dedicated Correlation
PTY all passed. The original 60/66 matrix and 0/6 affected-suite rerun inherited
NO_COLOR=1; the corrected full matrix passed 66/66 with NO_COLOR empty,
four workers and reported load 4.33. Rebase formatting and diff checks passed.

Separately, primary's pre-Correlation gate on `2992750` completed 57/67 with
the corrected harness and two workers. Ten failures differ from the earlier
six color failures: real, search-race, settings, canonical-view, bridge
diagnostics, command-enrichment, field-exploration, folding, shared-dialog
controls and text-time-basis. The log is
`/mnt/HC_Volume_106796581/lvu-build/primary-sol-harness-matrix.log`.
These results remain unresolved; the W21 worker pass does not erase them.
The exact primary app binary was preserved for paired diagnosis. No fresh
post-integration primary Rust or full-matrix pass is claimed here.


## 2026-09-08 — bounded wider assistance and source prompts integrated (W14)

Integrated worker commits `f4f0391`, `03e559c`, `04551e9`, `ffa6749` and
`2b95181` as `2ca89a3`, `cd55b96`, `f51acbc`, `3db394b` and `7bccc1c`.
The lib.rs conflict retained Correlation and whole-view statistics exports
alongside AskSample/AskSampleTier. Primary formatting and diff checks pass.

W14's locked Rust workspace tests and clippy passed, as did bridge build,
typecheck and 87 tests. Its original PTY matrix was 60/66 with inherited
NO_COLOR=1. The same binaries then passed all six affected suites sequentially
under the lock with NO_COLOR unset: real, canonical-view, details-colouring,
enrichment-chain, demo workflow and sixteen-colours. This is a corrected
six-suite rerun, not a newly executed full matrix. Rust and bridge tests were
not repeated solely for the environment correction.

Source prompts now describe discovery JSON rather than unrelated snapshot
and expression instructions. Short requests support one manual wider tier
with 2,048 total samples, 512 per source, 250,000 scanned records and 96 KiB
inline context. Shutdown settles the three assistance paths concurrently
before closing the shared bridge.

Primary reconciled the stale design-only sampling document with current
code. The displayed answer carries sample coverage, but widening clears the
earlier answer; the original proposal's persistent per-answer comparison is
still open. W14 was asked for that implementation seam and to strengthen a
shutdown concurrency test whose shared absolute deadline also lets sequential
settlement satisfy its elapsed-time assertion. Combined primary validation
remains pending; the separate pre-W21 ten-suite failures remain unresolved.


## 2026-09-08 — paired checks narrow two primary matrix failures

W13 ran canonical-view and command-enrichment sequentially under the shared
lock, explicitly unsetting NO_COLOR, against both the preserved pre-W21
primary binary and W21's binary. All four runs passed (6/7 seconds for
canonical-view; 4/5 seconds for command-enrichment). The test files and shared
harness were byte-identical between the failed matrix revision and the paired
checks. Binary checksums, exact commands and full transcripts are preserved in
`/mnt/HC_Volume_106796581/lvu-build/primary-sol-scratch/w13-paired-pty/`.
W13 performed no builds or source edits.

These two failures did not reproduce with either binary in isolation. The
remaining observed difference is concurrent matrix execution/shared-resource
interaction versus serialized direct execution; the checks do not establish
load magnitude or a specific race as the cause. The original 57/67 matrix
remains failing evidence, and the other eight suites were not checked here.

The combined primary gate on `0ad1026` stopped at compilation: primary's W14
conflict resolution had restored CorrelationControl/CorrelationDialog exports
from app after W21 moved those types into its component. Removing those stale
exports restores W21's intended public boundary while retaining Ask sample
and statistics exports. This was an integration error, separate from the
earlier PTY failures. The failed log remains
`/mnt/HC_Volume_106796581/lvu-build/primary-sol-w14-w21-gate.log`;
the corrected gate will use a separate log.


## 2026-09-08 — every future preview-worthy release is tagged

The user requested tags for every preview-worthy release from now onward.
AGENTS.md, the preview publication procedure and the release runbook now make
an annotated immutable tag on the exact validated source commit mandatory.
Development preview tags use `preview-<number>` (next `preview-058`); versioned
releases retain `vX.Y.Z`. The manifest records `source_tag`, and the tag must
be pushed and verified on origin before advancing `previews/latest`.

Primary checked the existing tag inventory (`v0.1.0`, `v0.1.1`), current preview
057 manifest, and release workflow trigger (`v*`). This change records the
prospective policy; it does not create a preview, move an existing tag or
change a published artifact. The current integration still requires its gate.


## 2026-09-08 — retain the current preview, retire superseded binaries

The user authorized stopping retention of previous previews. AGENTS.md and
the publication procedure now retain only the current preview binary after
successful replacement, with small manifests and Git tags kept as build
records. A running preview's removal is deferred. Captures and proof archives
remain protected.

Primary inspected previews 055 and 056: each contained only its binary and
manifest, each binary checksum matched the manifest, and neither had a live
process executable/cwd reference. Their manifests were copied byte-for-byte
to `previews/manifests/` before removing the two superseded binaries and old
directories. Removed binary sizes were 93,620,912 and 93,635,888 bytes
(187,256,800 bytes total).
Preview 057 and `previews/latest` remain intact. This was targeted preview
retirement, not a build/capture/proof cleanup.


## 2026-09-08 — capture buffer sharing under review (W25)

Worker `5cc4f8c`, based on the primary color-harness fix, shares immutable
RecordBytes backing across records from one read and reports framing/writer
CPU, commits and handovers to the soak harness. W25 reports targeted framing,
acquisition, journal, 37 ingest runtime tests and throughput passing; reported
measurements are 7.9 retained payload backings/MiB, 50.1 MiB per reader/writer
CPU-second, 2.0 commits/MiB and 2,339 records/handover. The full worker gate is
pending and this commit is not integrated.

Primary requested explicit invalid/missing/zero probe handling (current
truthiness checks skip zero throughput), full precision before computing
ratios, stable fixture identity rather than selecting the largest journal,
and a backing-lifetime/bounds audit with surviving-slice coverage. Final soak
evidence must account for the earlier query-p99 failure, missing probe and
nonzero restart exits.

Separately, the corrected primary W14/W21 gate passed compilation and earlier
workspace binaries but stopped in lvu-ingest runtime at 36/37: the small
throughput baseline timed out waiting five seconds for Stopped. The preceding
restart-race test passed. W25 is absent from this primary revision, so this
failure is not evidence against its implementation. It is also not yet
explained or resolved, and no complete primary workspace pass is claimed.


## 2026-09-08 — Extracted timestamp revision held for range compatibility

W19 returned `993413e` on `d593398` with five equivalence tests, live/export,
workspace tests, clippy and 67/67 PTY suites passing. It restores whitespace,
UTC aliases and second validation in a native Polars expression. It is not
yet integrated.

Primary review found that the new syntax mask accepts civil years 0001–9999,
but the old parser additionally rejects overflow while converting to i64
nanoseconds. A pinned Python Polars 1.44.1 probe of the corresponding nanosecond
strptime expression wrapped year 0001 into 1754 and year 9999 into 1816;
2262-04-11T23:47:16.854775808Z also wrapped across the signed boundary.
This probe is evidence from Python, not an actual Rust read_extracted result.
W19 was asked to reproduce through the Rust expression and preserve the old
checked conversion, including fractions and offsets at both bounds. The
five-test suite covers rejected year 0000 but not these range boundaries.
The passing worker gate remains evidence for that revision, not proof of
complete timestamp-profile equivalence.


## 2026-09-08 — ANSI presentation revision under review (W31)

W31 returned `113c747` with focused sanitizer/TestBackend tests, app build,
lvu clippy and actual PTY passing. The PTY checks clean logs and Details,
literal `[2m` retention, terminal restoration and original ANSI bytes in the
journal. A first Unicode assertion was corrected for pyte's composed cell
representation. This revision is not integrated.

Primary found that collapsed fold patterns bypass the sanitized event-line
renderer, and Details parses sanitized JSON while app navigation still parses
the original text. W31 was asked to align these display/navigation paths
without changing captured/query values, test folded and ANSI-wrapped JSON
interaction, remove the PTY's hard-coded host scratch path, and cover ANSI
escape intermediates and string controls beyond CSI/OSC. The broader W22
multiline grouping assignment remains separate.


## 2026-09-08 — base-screen source keys integrated (W27)

Integrated worker `15057e6` directly on its primary base `7a82ec1`. `X` stops
and `R` restarts the selected source in Logs or the sidebar, with Alt-S/Alt-R
retained as aliases. Help and palette advertise the bare keys; `S` continues
to open Storage. The worker's Correlation rebase preserved the removed legacy
focus and changed only the source-key behavior. This is not yet published.

Worker evidence before the material Correlation rebase: full workspace Rust
tests, clippy, builds and formatting passed. After that rebase, formatting,
fresh builds and 178 targeted tests passed (11 discoverability, 3 Help,
164 UI-state), followed by 68/68 PTY suites in 123 seconds at four workers
and reported load 5.74. The new PTY drives stop/restart from both panes at
80x24, preserves Storage, and checks the journal for repeated records.
The earlier 61/67 matrix and 0/6 serial rerun inherited NO_COLOR=1; the
corrected 68/68 run is the passing PTY evidence.

Primary cancelled its own queued gate before lock acquisition to integrate
W27 without changing sources beneath a running gate. No active build/test
was interrupted. Combined primary validation will run on the updated source;
the prior ingest timeout and ten-suite matrix failure remain recorded.


## 2026-09-08 — color candidate rejection needs diagnostic provenance (W18)

W18 returned `b76c4fd` on `d985090`, replacing silent compiler/parser/runtime
rule skipping with whole-candidate rejection and adding managed charges for
retained/new color-match entries. The worker reports locked targeted tests
for rejection, accounting, component diagnostics and SQLite reopen passing;
no PTY result is claimed.

Primary found a new regression in runtime error routing: it classifies every
numeric diagnostic field as a color-rule index, without checking error state.
The engine permits numeric enrichment names and emits Ready diagnostics for
them, so a successful stage named `0` would be rejected as a color error even
without color rules. W18 was asked for explicit color diagnostic provenance,
numeric-field regression coverage, and cap-pressure/live-append preservation
checks beyond the accounting delta. W28 was notified to hold an unacquired
combined gate until the amended dependency is ready. No active gate is to be
interrupted. This revision remains unintegrated.


## 2026-09-08 — ANSI projection integrated; primary gate remains red

Integrated amended W31 `815249a` as `99340c9`. Collapsed fold patterns now
sanitize before clipping; expanded groups sanitize before splitting. Details
keeps original JSON structure consistent with navigation and sanitizes display
text only. Escape intermediates and DCS/SOS/PM/APC strings join CSI/OSC
coverage, and the PTY honors tempfile/TMPDIR rather than a host-specific path.
Worker targeted Rust/TestBackend, clippy, app build and actual PTY passed,
including original bytes in the journal and terminal restoration. This is
local, not published; broader multiline recognition remains W22's work.

Before this pick, the primary gate on `d985090` completed all stages:
isolated ingest throughput passed; workspace stopped again at the same
36/37 ingest runtime timeout; clippy passed; bridge typecheck completed but
tests were 78/87; bridge build passed; PTY matrix was 48/68. The full log is
`/mnt/HC_Volume_106796581/lvu-build/primary-sol-w27-combined-gate.log`.
This does not establish a clean combined gate.

Primary noticed an additional possible confound in earlier paired PTY checks:
its gate explicitly places all fixture temp directories on the build volume
via TMPDIR, whereas the paired report named only transcript placement. W13
was asked to verify actual temporary-directory settings and compare fixture
storage location and concurrency independently. No I/O or scheduling cause
is claimed until that comparison completes.


## 2026-09-08 — checked Extracted conversion and bounded Ask history integrated

Integrated W19 `4110373` as `43e0678`. Both date spellings parse whole seconds
at millisecond precision; fractional digits are scaled separately and unsafe
seconds are nulled before eager nanosecond arithmetic. This preserves the old
multiply-before-add lower limit as well as the exact upper limit. Worker
focused Rust equivalence tests passed 7/7 and live/export passed, covering
years 0001/9999, boundary fractions and offset crossings. No exhaustive date
equivalence is claimed. The older 67/67 PTY result remains tied to `993413e`.

Integrated W14 followups `29141b4`/`01a788c` as `397f952`/`62547da` after the
active ANSI gate finished. The shutdown concurrency test checks thread
identities and channel ordering. Ask keeps one immutable previous answer in
the existing Proposal pane, including its sample and recipe presentation;
failed wider retries preserve it, new requests clear it, and Apply uses the
current candidate only. Worker evidence: concurrency test 1/1, Ask tests
13/13, targeted clippy and real-source PTY with narrow scrolling passed.

Primary's ANSI gate began at `58951c5`: focused tests passed 5/5, lvu clippy
and app build passed, but the PTY timed out after three seconds with no rows
and capture reporting zero records. W19 was picked while that gate was active,
before its app build completed, so its header does not identify one immutable
source revision for every stage. This log is stage evidence, not a clean
combined gate: `/mnt/HC_Volume_106796581/lvu-build/primary-sol-ansi-gate.log`.
Future acceptance must use the final unchanged source.

W25 returned revised `2cb59de` with strict probe validation, stable fixture
identity and ownership bounds; focused validation is pending. Its earlier
full gate on `5cc4f8c` passed Rust/clippy and recorded query p99 .955, a probe,
and zero restart exits, but failed RSS growth (69.7 MiB) and capture throughput
(9.3 combined / 15.9 writer MiB per CPU-second). Those failures remain open;
primary requested matched-base diagnosis without relaxing the thresholds.


## 2026-09-08 — fixture storage isolated as a gate failure cause

W13 corrected the earlier paired-test inference: its prior runs did not export
TMPDIR and used /tmp on /dev/sda1, while primary placed fixtures on the mounted
volume /dev/sdb. With the same preserved binary and unchanged tests/helper,
both canonical-view and command-enrichment passed on /tmp serially and
concurrently. Canonical-view failed on a dedicated volume root even serially
at autosave shutdown; command-enrichment passed there but took 14/21 seconds.
With primary's exact volume TMPDIR, canonical-view failed at filter apply or
persisted-view restoration and concurrent command-enrichment timed out exiting.
NO_COLOR was unset and all runs held the validation lock.

Fixture filesystem/location is therefore a demonstrated causal axis;
concurrency is not necessary for the canonical failure. The earlier instant
No-filter disappearance was not specifically reproduced. The former claim
that concurrency was the sole remaining difference is withdrawn. Full logs:
`primary-sol-scratch/w13-2x2-pty-20260908T233805Z` and
`primary-sol-scratch/w13-exact-tmpdir-pty-20260908T234313Z` under the build
volume. W13 removed only its synthetic fixture roots and retained transcripts.

Primary restored short-lived fixture placement to dedicated /tmp roots with
space preflight, explicit ownership and gate-exit cleanup; build targets,
caches and durable evidence stay on the volume. Root has 27 GB free at this
check. Slow-volume save/flush/shutdown behavior remains an observed limitation;
returning fixtures to their original filesystem is not a product fix for it.

The focused primary gate at `db93614` passed Ask components, deterministic
shutdown concurrency, Extracted boundaries, live/export and workspace clippy.
Its source stayed unchanged through the gate. This focused pass does not
replace the pending full Rust/bridge/PTY validation on corrected fixture storage.


## 2026-09-08 — color rules integrated; search optimization pending

Integrated corrected W18 `0f05235` as `7e0fa22`. The engine now returns color
diagnostics separately from unrestricted enrichment field names and only
actual color errors reject the candidate. Worker regressions cover numeric
enrichment names, cap pressure, retained copies and live append after rejected
rules. ANSI cleanup precedes both highlighting and clipping. The lib.rs
conflict retained AskAnswer and added ColorRule/RuleColor exports. Primary
formatting and diff checks pass; W28 will validate its summary on this current
integration using the corrected fixture filesystem.

W24 returned `882ac4f` on `db93614`, still unintegrated pending its post-rebase
gate and reconciliation with W18's shared TextSearch engine path. Reported
same-test CPU throughput was 6.89M records/s for regex versus 4.77M for
lowercase (6M guard); 620k/3M engine rates were 1554/1519 versus 542/467 MB/s.
Mixed batches partition ASCII rows for escaped ASCII-insensitive regex and
retain lowercase semantics for non-ASCII rows, then restore original order.
These are worker benchmark results, not a completed combined validation.

## 2026-09-09 — ordering fixture and matrix build ordering corrected

The primary gate on `e6dae0e` with local /tmp fixtures passed formatting,
bridge typecheck, all 87 bridge tests and bridge build. Rust workspace tests
and clippy stopped at the ordering-test Membership initializer missing the
new color_matches/color_rules fields. The matrix did not execute: its
freshness prerequisite ran concurrently with its builds and rejected stale
binaries before the rebuild completed. Log: `primary-sol-local-fixture-gate.log`
on the build volume. No full-gate acceptance is claimed.

Primary added empty color state to the ordering fixture and moved the matrix
freshness check into its run phase, after both binary build dependencies.
Formatting and diff checks pass; Rust and matrix validation are pending.

W24's current candidate is `bb2fe9d` on `7e0fa22`. Worker query tests, Unicode
and color-rule integration coverage, and scan throughput passed. Clippy hit
the same primary initializer error; builds and matrix were not run. The
optimized guard measured 5.64–6.89M records per CPU-second versus 4.77M with
the fast path disabled. Its revised floor is 5.2M, replacing the earlier 6M
floor after the observed 5.64M enabled result. Full integration remains pending.

## 2026-09-09 — full primary gate and shared compiler scratch repair

On unchanged `d85807e`, the corrected local-fixture gate passed workspace Rust
tests, workspace/all-target clippy, formatting, bridge typecheck, 87 bridge
tests and bridge build. The matrix ran after both builds and passed 68/70;
real-source and Settings suites failed. Their failures remain unresolved;
the matrix's abbreviated output does not establish their causes. Log:
`primary-sol-ordering-fix-gate.log` on the build volume.

After that gate removed its disposable fixture root, W28 compilation failed
because shared sccache still had TMPDIR=/tmp/lvu-primary-gate-h82ndp_o in its
server environment. Primary confirmed that environment directly. Added
`scripts/sccache.sh` as the default Rust wrapper, preserving explicit caller
overrides, to keep compiler/server temporary files under SCCACHE_DIR/tmp while
test processes retain disposable /tmp fixtures. Manual preflight cache access
also uses this wrapper. Shell syntax and argument/exit/environment isolation
checks pass. Server repair was coordinated with W24/W28 holding and W25 past
compilation in a soak with RUSTC_WRAPPER empty; no compiler was active.
The old server stopped cleanly; the replacement's environment names persistent
volume scratch. Two real Rust library compilations passed through the wrapper,
including after removing the smoke check's own caller TMPDIR. Synthetic files
were cleaned up and no other gate fixture was recreated. Repair log:
`primary-sol-sccache-repair.log` on the build volume.

## 2026-09-09 — corrected capture measurements; soak still fails

W25 candidate `4193ed8` on `d85807e` remains unintegrated. Its corrected
capture-only counters exclude journal-page reads from writer CPU. The final
volume-backed soak records 48.43 source MiB per reader+writer CPU-second,
88.22 journal MiB per writer CPU-second, 1.29 commits per journal MiB and
1685 records per handover, passing unchanged capture thresholds. Within-process
steady RSS growth is 2.7 and 0.1 MiB; the earlier 69.7 MiB comparison crossed
process restarts and does not establish a retained-memory leak.

Matched ext4 /dev/sdb A/B used the same 67,108,993-byte fixture and settings.
The original all-writer-thread measurement is retained separately: shared
reader/writer CPU .0946/5.5605 seconds, versus .2245/6.9912 for per-record
copies. Shared backing improved combined throughput 27%, writer throughput
26%, and reduced reader CPU 58%. These all-thread values must not be compared
directly with the later capture-only writer denominator.

The final soak remains red: the first SOAK_MARKER query took 12.921 seconds
(1.74 process CPU seconds), followed by 35 queries at .55–1.52 seconds. Both
exits returned 1 with memory autosave flush deadlines; the second also reported
three bridge-worker shutdown deadlines. Raw evidence: `w25-final-soak.log`
and `w25-final-report.json` on the build volume. Earlier false-settle and
900-second wait failures remain in `w25-corrected*`; historical p99 .955 and
clean restarts are not acceptance for this final run. Path ownership alone
does not prove these failures are independent of W25.

Worker focused evidence includes ten Python fixtures, writer-page accounting,
app build, workspace all-target check and isolated ingest throughput 37.59
MiB per CPU-second; previous full Rust/clippy results belong to earlier
revisions. Primary requested stronger boundary tests through raw probe inputs
and capture accounting through actual append/commit before final amendment.
W14 is tracing assistance shutdown; W13 is reproducing the two remaining
primary PTY failures with exact binaries and full transcripts. No preview is
accepted from this evidence.

## 2026-09-09 — shutdown diagnostic follow-up

W22's revised status `1baf29b` and FOLLOW `a356243` remain unintegrated;
earlier targeted failures were corrected but the final gate stopped at disk
preflight. Review confirms served-row identities now determine the anchor and
tail selection. Remaining status review covers the raw-context return shortcut
beside long actions and event-time clauses following another diagnostic.

W14 is implementing bounded teardown for bridge startup before Node registers
EOF handlers, including a deterministic delayed-start child. Memory flush is
being diagnosed separately: its 500ms budget follows assistance teardown, and
both W25 exits failed memory flushing while only one reported bridge failure.
No causal dependence between those failures is established.

Primary added bounded worker-phase snapshots and pending/inflight counts to
memory-flush errors, distinguishing final-save submission, flush queue admission
and flush acknowledgement. A full command queue at deadline previously reported
"disconnected" even while connected; it now reports the admission timeout.
The budget and persistence behavior are unchanged. New tests exercise actual
save durability through saturated command/event queues and both timeout stages.
Formatting and diff checks pass; focused Rust/clippy validation is pending.

## 2026-09-09 — disk recovery, wrapped PTY assertions and capture integration

Disk preflight blocked W22/W24/W28 and the primary memory diagnostic gate before
new compilation. Normal janitor and stale-flags dry-run found no reclaimable
artifacts. Primary inspected the completed W23/W26 Cargo targets, confirmed no
live process references, and dry-ran Cargo cleanup. Under the shared lock,
`cargo clean --profile dev` removed only those two debug profiles (3.6 and
2.0 GiB). Other target-root metadata/tmp, source worktrees, reports, captures,
previews and release assets were retained. Free space rose by 5,813,936,128
bytes to 11,027,509,248; disk preflight passes. Evidence:
`primary-sol-finished-target-cleanup.log` on the build volume. Workers were
notified to resume their remaining validation.

W13 preserved the exact d85807e primary app (SHA-256
`cfa4822c89f33824e2a9920be70ba9e9b51d4bb448878f8c756b5b066b8fe8fe`).
With unchanged tests, NO_COLOR unset and local /dev/sda1 fixtures, long TMPDIR
names reproduced both remaining PTY failures; a short TMPDIR made both pass.
The real-source test required contiguous "not installed or not on PATH" despite
visible line wrapping. Settings required a contiguous absolute path despite its
wrapping within settings.toml. This controlled comparison establishes path-length
sensitive assertions for these two failures, rather than product/load/timeout
causes. Full logs and binary: `primary-sol-scratch/w13-d85807e-diagnostic/`.
W13 is correcting the assertions while preserving full remedy/path checks.

Integrated W25 amended `bb48da8` as `9744a89`. Its strengthened tests now
derive near-boundary metrics from raw recordings and follow real page service
with actual append/commit. Primary formatting, diff checks and all ten Python
soak fixtures pass. Combined Rust validation remains pending; the earlier
primary memory gate stopped at disk preflight and ran no Rust tests. Latest
W25 cold-query and shutdown failures remain open. W22 status/FOLLOW are now
`36e1d77`/`14bf9a5` on `34baf29`, still awaiting final corrected validation.


## 2026-09-09 — capture/memory Rust gate and summary/search integration

Primary `826704a` passed formatting, workspace Rust tests with no-fail-fast,
workspace all-target clippy with warnings denied, and app/TUI builds under the
shared validation lock. This includes W25 shared backing and primary memory
phase diagnostics plus saturated-save durability tests. Evidence:
`primary-sol-capture-memory-gate.log`. It does not resolve the volume-backed
cold query or shutdown soak failures. W25 is preserving that exact app for a
bounded runtime diagnostic using the new phase/count errors.

Integrated W28 `c2b0633` as `8f28148`. Worker summary components passed 5/5,
dedicated summary PTY passed, and builds/clippy passed. Workspace tests had
one empty-PID parse in command cancellation; its exact rerun passed. Matrix
69/71 retains real-source and Settings wrapping-sensitive failures. Primary
inspection found that the delivery fixture creates its final PID path before
writing its contents, while reservation waits only for file existence before
cancelling the child. W28 owns a separate atomic-publication fixture correction;
the passing rerun alone does not close this race.

Integrated W24 `7894576` as `7b8ce95`. Worker query tests, scan throughput,
clippy and app/TUI builds passed; matrix 68/70 retains the same two harness
areas. Eight-repeat matched search throughput was 6.526M versus disabled-path
4.863M records per CPU-second, with a fixed 5.6M guard. Primary adapted the new
throughput fixture to the integrated RecordBytes type. W13 is checking corrected
wrapped assertions against preserved primary and W24 binaries; final integration
validation has not yet run. No preview acceptance is claimed.


## 2026-09-09 — current preview tag published

Created and pushed annotated `preview-057` on the existing current preview's
recorded source `618b19ba65640ea3d9489369c55ce871c89695ea`. Verified the copied
binary SHA-256 `657db3f898127ddb8af0a104af78c3613721e80ec4ab9659a4a6699bfec3bb0d`
and remote peeled tag. Annotation records the original checks and limitations;
this does not claim a new build or acceptance of today's integration. The
published binary, original manifest and latest symlink are unchanged. A small
supplemental manifest records source_tag. The next new preview remains 058.


## 2026-09-09 — preview 058 published and tagged

Published `058-view-controls-and-search`, annotated tag `preview-058`, from
clean source `7e5b5c53c963dbd6245b07b0fe9f37ea78aa0305` on the integration
branch. Binary SHA-256:
`7680c1129c329e672f68b956a33ce407b83bca178f39eb418a58b92895e8647b`.
The explicit tag push succeeded and origin's peeled tag matches that source.

Current-source validation under the shared lock: formatting, 1,243 Rust tests
passed (seven intentionally ignored), workspace all-target clippy with warnings
denied, app/TUI builds, and 71/71 PTY suites in 125 seconds at four workers.
W13 wrapping correction `9e235a8` was integrated as `7e5b5c5`; both previously
failing suites now pass in the full matrix. The copied preview separately passed
--help, View Summary PTY and ANSI presentation PTY; its checksum was verified
before and after testing. Logs: `primary-preview-058-final-gate.log` and
`primary-preview-058-copy-acceptance.log` on the build volume.

This preview includes summary, colour rules, ANSI presentation cleanup, search
acceleration, Extracted-time compatibility, Correlation, source X/R keys, wider
Ask/history and shared capture backing. W22 status/FOLLOW and generic multiline
work are excluded. The prior volume-backed cold-query/shutdown soak failures
remain unresolved and are recorded in the manifest; no full-soak pass is claimed.
A separate PID fixture-publication race correction remains pending. Earlier
package-scoped preview validation was stopped to avoid building another Polars
feature set; only the completed workspace gate supplies acceptance evidence.

The current launcher advances to preview 058 after tag verification. Retire only
the superseded preview 057 binary if no process uses it; preserve its manifest,
all capture data and proof archives. The next new preview number is 059.


## 2026-09-09 — follow-up integration after preview 058

Integrated W28 atomic PID fixture `13c3ba2` as `699814a`. The helper closes a
process-unique sibling PID file before os.replace publishes readiness. Strict
PID parsing, cancellation, no-delivery and reap assertions are unchanged.
Worker locked evidence: protocol 15/15 and former race 20/20 exact repetitions;
formatting and diff checks clean. Reused paths belong to already-attempted
retries that do not spawn. Primary targeted confirmation follows; preview 058
remains immutable.

W14 `5332f09` remains held after correcting delayed-start request/EOF ordering.
Worker typecheck and bridge 92/92 pass; two focused Rust regressions pass.
Primary's actual lifecycle/server probe with a synchronous response stub and two
staged chunks found a further EOF/backpressure deadlock: after 250ms with a 20ms
drain budget, zero bridge closes, no diagnostic, and 67 unread staged bytes.
The first response paused staged input, preventing end and therefore preventing
the shutdown timer from starting. This is a lifecycle/server probe, not an
end-to-end backend result. Worker is confirming the same ordering with a real
server parse-error response followed by valid input and correcting bounded
settlement, including input-error stop paths, without extending the host budget.

W22's six non-wrapping matrix findings were reviewed against exact screens.
Four assertions collided with deliberate compact status presentation; the base
lvu diagnostic invoked the wrong binary and is invalid evidence. Gap navigation
lost its just-completed action notice, a real status-priority regression. W22 is
fixing that and retaining applied-state/membership assertions in targeted PTYs
before another full matrix. Status/FOLLOW and generic multiline remain outside
the published preview.


Primary confirmation on `0d15f63`: formatting, the complete protocol suite
15/15, and workspace all-target clippy with warnings denied passed under the
shared lock. Log: `primary-pid-fixture-gate.log`. No product binary changed and
preview 058 was not rebuilt or retagged for this fixture-only correction.

W25 preserved-binary diagnosis on `826704a` (SHA-256
`107c73db5414769c376befbb6d0d37165b0f5069508a3120870fe7b67682fd57`)
used ext4 /dev/sdb without builds or source edits. Canonical PTY failed before
shutdown at a 10.00-second filter wait. One bounded short soak reproduced exit 1:
memory-flush 0.502 seconds, worker phase `persisting view`, pending=0, inflight=1;
total shutdown 2.324 seconds, including assistance 0.755 and capture cleanup
1.064 seconds. Query p99 was independently 1.514 seconds; capture/RSS checks
passed (49.73 combined capture MiB/CPU-second, 90.68 writer, RSS growth 1.3/0.4
MiB). Evidence: `w25-shutdown-diagnosis/` canonical/short-soak logs, report JSON
and provenance. A pending save remained inside persistence; this does not yet
distinguish SQLite lock contention, serialization work or storage flush latency.
A bounded syscall diagnosis is assigned before changing deadlines or durability.


## 2026-09-09 — user replaces preview publication with versioned releases

The user explicitly retired previews: only the existing vX.Y.Z version scheme
is permitted. Preparing v0.1.2 with matching app/lockfile versions, the existing
four-target release workflow, checked archives, tag and public release. Removed
the preview launcher/policy in favor of versioned installation and `mise run lvu`.
Historical ledger evidence is retained; obsolete preview tags/artifacts will be
retired after the versioned replacement is available. No new preview tag will
be created. W26 owns archive/tap verification and coordinated publication; primary
owns the version tag and shared documentation.

Bridge shutdown is integrated as 86aa9bd, including primary corrections that
wait for actual upstream EOF before overriding backpressure and handle input
errors during startup. TypeScript typecheck and 96/96 bridge tests pass. The
queued primary Rust gate was cancelled before acquisition so the version bump
and all release checks can share one coherent v0.1.2 gate. Its evidence does not
yet claim a Rust pass for this revision.


Version 0.1.2 gate on `3017970`: workspace Rust tests, formatting, workspace
all-target clippy, app/demo builds and bridge typecheck/96 tests/build passed.
Matrix did not start: the guard correctly noticed the old demo binary mtime
predated the changed root manifest/lockfile, although Cargo considered that
unaffected binary fresh. Force rebuilding the demo from unchanged source content
will refresh its real build provenance; no binary timestamp will be fabricated.
The following policy edits are documentation-only. Matrix acceptance follows on
the final source commit, without repeating unchanged Rust/bridge checks.

The matrix on d1d84ff ran and passed 70/71; the daemon-unreachable diagnostic
failed and reproduced alone. The new startup stdin staging kept Node alive after
connection failure, so the host never received process EOF and Ask stayed pending.
CliLifecycle now releases its owned input and staged stream on startup failure,
preserving the original connection error. Typecheck, 96/96 bridge tests and the
actual bridge-diagnostics PTY pass after correction; no timeout was increased.
Logs: primary-v0.1.2-final-matrix.log, primary-v012-bridge-diagnostic.log and
primary-v012-bridge-diagnostic-fixed.log. Full corrected matrix follows; unchanged
Rust retains the 1,245-pass/seven-ignored gate evidence above.

Corrected release candidate `207b95f6ece127dcfda5870fd86d69586bc90ed9` passed
71/71 PTY suites in 122 seconds with fresh app/demo binaries and the built
bridge. Annotated `v0.1.2` was pushed and remote-verified: tag object
`a80fec2ecda11895c19de3f732ce995870a5d59a` peels to that source. Native four-target
release run `34303930416` is building the archives. A pushed tag is not yet a
public downloadable release; archive/hash verification, tap update and undraft
remain assigned to W26. W22's status/FOLLOW gate hold is released for subsequent
version work; its pending changes are outside v0.1.2.

W25's instrumented ext4 follow-up found no SQLite lock waiting: all 710 fcntl
calls succeeded, maximum 0.205 ms. The slowest save transaction was 410.129 ms,
including 351.004 ms in synchronization and 0.039 ms acquiring the initial write
lock; the slowest fsync was 286.234 ms. Both shutdowns completed in this traced
run (maximum 0.448 s), so the earlier 500 ms failure did not reproduce and its
cause is not conclusively established. Tracing raised capture wall time from
70.3 to 149.2 seconds and distorted writer CPU/RSS sampling; these are diagnostic
observations, not performance acceptance. Original binary identity is unchanged.
Evidence is under `w25-shutdown-diagnosis/`: strace-soak-volume log/report and
memory-save.3566862 / memory-save.3576922. No timeout or durability change was made.

## 2026-09-09 — v0.1.2 publication completed

Release `385195581` became public, non-prerelease and latest at 03:02:47 UTC:
https://github.com/indigoviolet/lvu/releases/tag/v0.1.2. Run `34303930416`
completed successfully for all four native archive jobs and publication. The
annotated tag remains `a80fec2ecda11895c19de3f732ce995870a5d59a`, pointing to
`207b95f6ece127dcfda5870fd86d69586bc90ed9`. No main merge or tag movement occurred.

W26 compared API digests, every downloaded archive and SHA256SUMS, inspected the
correct target roots and bundled resources/licenses, then rendered the formula
from those checksums. Style, strict audit and exact renderer comparison passed.
Formula-only tap commit `8da3e55a136e505a781f02e76f61e4eb09da22a7` was pushed and
remote-verified before the draft was made public. Public asset endpoints work;
primary independently verified latest/public state, asset digests and tap HEAD.

| Target | Bytes | SHA-256 |
| --- | ---: | --- |
| aarch64 macOS | 25464443 | `64c76518dc7c5cdc7c410ca9811160736466658de404f20ff3ff02819ee2506a` |
| aarch64 Linux musl | 25745631 | `86953ae4ad185059f95c28b03db553a94d5dd4e0665db0cc8da26467391b4e58` |
| x86_64 macOS | 27244768 | `6185faa2d50ad932493469fd32edd2e704abf954f3273d09cf40ff95ece76836` |
| x86_64 Linux musl | 27888994 | `7d68e047972e81fa3b057f8ed9e2bdd4dffc5cfe007bf745428645eda14c2cbd` |

Primary installed the official Linux archive at `versions/v0.1.2`, verified its
checksum and binary SHA `56a5ee6bb0f21ac705e5402f08a8392b6f61a57c72c7277245ed2b14cc7712f1`,
and tested direct/symlink help and installed-resource resolution outside the
checkout. The packaged helper compiled a real Polars expression, the packaged
bridge answered capabilities and exited after EOF, and View Summary/ANSI PTYs
passed under the shared lock with local disposable fixtures and NO_COLOR unset.
Log: `primary-v012-installed-acceptance.log`. `versions/latest` now selects
v0.1.2 and `mise run lvu -- --help` passes. Clean Homebrew/mise install evidence
remains explicitly historical v0.1.1 evidence; no new human Mac/arm64 claim.

Removed the two obsolete development tags locally/remotely and retired the
superseded local runtime after confirming no process used it. Historical build
manifests remain in `versions/records`; captures and proof archives were untouched.
Release notes preserve unresolved volume-backed cold-query/autosave-shutdown
failures and do not claim a full-soak pass or general capture performance result.

## 2026-09-09 — next-version status/FOLLOW integration

W22 status `7bed7e8` integrated as `3f0eda3`; FOLLOW `dc65ee1` as `ed83a68`.
Requested HISTORY top and served top are separate, so retained rows cannot
abandon a navigation destination; status ranges and hitboxes use served identity.
Worker validation: status 17/17, UI 165/165, live-view 45 passed/two intentionally
ignored, six corrected semantic PTYs, dedicated FOLLOW and whole-view statistics
PTYs, final matrix 72/72 in 133 seconds. Primary confirmed the integration differs
from worker content only in the ledger, then passed status/UI 182/182. Workspace
clippy caught an obfuscated-if-else lint in status rendering; an equivalent
explicit if/else fixed it, and formatting plus workspace all-target clippy passed.
Logs: `primary-w22-integrated-gate.log`, `primary-w22-clippy-corrected.log`.
These changes are outside the immutable v0.1.2 tag and installed runtime.

W22 now owns generic multiline work through existing Grouping/Folding, preserving
original bytes/IDs with conservative bounded event boundaries. The proposed Auto
token must be invalid legacy regex syntax so existing valid custom rules retain
their meaning; unknown token versions must reject actionably. No generic multiline
implementation or acceptance is claimed yet.

## 2026-09-09 — multiline review and raw-jump status correction

W22 candidate `f9441e1` is not accepted. Focused worker gates pass, but primary
review found boundary/UI/accounting gaps: indented credible event headers are
classified as continuations; Auto's internal token can be edited by cursor/kill
commands; GroupRange growth still uses the old 64-byte charge; expansion includes
group_line_count as if it were a member. Further regressions requested cover a
backward timestamp still after the head timestamp, and arbitrary partial-line
Start/End provenance leaving multiline state open after an unrecognized line.
Worker owns correction before a final matrix or integration claim.

The three broader lvu test failures reported by W22 reproduce on accepted-base
product content without the multiline commit. Two are stale wording assertions:
the external-command dialog says new records stay pending until explicitly run,
and landed raw context now presents a compact `o back` independently of its full
origin label. The pending-jump failure is a real status regression: the narrow
empty pane showed HISTORY and 0-0/0 without explaining that record location was
pending. The protected prefix now says `locating #N` until the chase resolves or
reports its bounded failure. Full origin context remains optional when space fits.

Primary validation: component enrichment 13/13, raw context 6/6, discoverability
17/17; formatting, workspace all-target clippy, app/demo builds and actual raw
context PTY pass. Tests retain pending-command guarantees, anchor/neighbor IDs,
bounded unresolved chase, return state and wide origin context, and check the
compact locating notice at 80/100/160 columns. Logs:
`primary-w22-baseline-assertions.log`, `primary-raw-jump-notice-gate.log`.
The immutable public v0.1.2 archive is unchanged.

## 2026-09-09 — corrected multiline candidate gate and final review

W22 candidate `a3eb19a` on `f1ae7aa` passed lvu-view and lvu Rust suites,
workspace all-target clippy, app/demo builds, dedicated multiline and real-source
PTYs, and the full matrix: 73/73 in 133 seconds. Two benchmark tests remain
intentionally ignored. The worker used its own volume target, the shared lock,
persistent compiler wrapper, a short marker-owned local temporary root and
NO_COLOR unset. The worktree is clean. This is candidate evidence, not integration
or publication acceptance. Earlier `f9441e1` and `733f2ea` remain superseded.

Primary source review found three remaining concrete cases: selecting Custom
unconditionally replaces an existing regex with the default; a partial chain can
retain provenance after a stream, adjacency or time boundary when starting a new
group; and leading ANSI styling is skipped for head classification but not for
structured-state initialization, so a coloured JSON head may remain open after
its closing bracket. W22 owns the bounded corrections and regression evidence.
The current installed and published version remains v0.1.2 unchanged.

## 2026-09-09 — multiline implementation handoff and Muse routing

W22 ended its Sol implementation session clean at `6ee22cf`. The three final
review corrections cover Custom mode preservation, partial-chain boundary reset
and ANSI-prefixed structured closure. Focused regressions, complete lvu-view
and lvu suites, workspace all-target clippy, app/demo builds, multiline PTY
and real-source PTY passed. The first final matrix was 72/73: fold-by-column
failed once and passed an immediate isolated rerun with the same binary and
environment. A second matrix ended without a recoverable summary; no pass is
claimed. Historical `a3eb19a` 73/73 remains evidence only for that revision.
The corrected candidate is not integrated or released yet.

The user selected Muse Spark 1.3 for subsequent implementation subagents while
retaining this primary as supervisor. The configured profile passed a read-only
repository/command smoke test. Sol W22 holds all further work; future fixes and
validation route to Muse. The TODO/changelog implementation is assigned to a
dedicated Muse worktree; primary retains review, integration and publication.

## 2026-09-09 — active work plan and version changelog

Muse Spark implementation `1328b6c` reorganized TODO into ongoing work, accepted
next-release changes and upcoming investigations. All eleven previously
unfinished items remain, plus the accepted pending raw-context notice. Completed
historical rows moved to docs/todo-history.md with their relocated ledger link;
primary verified the archived section is otherwise unchanged.

CHANGELOG.md records Unreleased and v0.1.0–v0.1.2 under Features, Fixes and
Breaking changes. Primary compared GitHub publication notes/dates and relevant
tag history; v0.1.1 explicitly records default resume and the retired p shortcut.
Review removed unproven version attribution and a downgrade-safety inference.
Multiline remains ongoing rather than an accepted changelog feature, and the
volume-backed soak limitation stays explicit. README/versions link the changelog;
the release runbook now requires updating it when accepted changes land and
moving Unreleased entries into the actual version at publication.

Validation: 23 local Markdown links resolve; historical archive comparison,
active-row inventory (one Working, three Ready, eight Open), four categorized
changelog sections and diff whitespace checks pass. Documentation only; no
Rust/PTY build, release tag or installed-version change.

## 2026-09-09 — parallel Muse work and retired-target cleanup

The user requested maximum parallel progress, with the primary retained as
supervisor. Six Muse Spark 1.3 implementers now have bounded independent work.

| Work | Agent | Workspace |
| --- | --- | --- |
| Multiline acceptance | cd5b5729-cb1c-45f9-9a1c-ad3835e4835e | lvu-w22-folding-blank (exclusive handoff from stopped Sol) |
| Memory autosave shutdown | 0523e863-b8b7-405e-a49e-0376c0d4a295 | lvu-muse-autosave |
| Blank viewport | 8b525718-a120-4638-adfe-39d8265a85e6 | lvu-muse-viewport |
| Missing enrichment columns | a8f5e7f1-56c3-49e3-9b7e-f8e03cd87a72 | lvu-muse-enrichment |
| Fields PTY stability | 62af14ba-1247-47b2-b0a7-00f6cc088c46 | lvu-muse-fields |
| Cold-query journal scheduling | fc42914a-c8d6-482a-bd56-d61a33afba89 | lvu-muse-journal |

Source analysis, fixtures and edits proceed concurrently in disjoint ownership.
Costly gates retain the shared validation lock; multiline has first matrix
priority. Fresh worktree targets remain isolated, with shared compiler caching.

Disk inventory found twelve retired Cargo targets using roughly 51 GiB; the
installed v0.1.2 is only 100 MiB and no previews directory remains. Under the
shared lock, primary verified no target symlinks, held Cargo locks or live
process references, then used cargo clean with an exact twelve-target allowlist:
W13, W15, W21, W27, W25, W24, W31, W19, W14, W18, W29 and W28. Measured free
space rose from 6.37 to 57.17 GiB, reclaiming 50.80 GiB. Primary and W22 targets,
compiler cache, installed release, captures and proof were preserved. The exact
commands/output are in retired-target-cleanup-20260909.log on the build volume.
The supervisor will retire completed-worker build output after integration and
any required diagnostic binary preservation, rather than retain every target
indefinitely merely because its branch was not merged into main.

## 2026-09-09 — v0.1.3 primary gate and source-discovery answers

Accepted multiline `6ee22cf` was integrated as `e40b386`; version preparation
`9176e6b` changes the application version to 0.1.3 without dependency upgrades.
Muse measured 73/73 on the exact worker candidate. Primary then independently
ran formatting, complete workspace Rust tests, workspace all-target clippy,
bridge checks and application/demo builds successfully on `9176e6b`.
The primary matrix was 72/73 in 135 seconds: generic multiline timed out after
three seconds waiting for Mode Auto. No failing transcript was retained by that
test. Three isolated runs of the identical primary binary and test passed; this
does not explain the matrix failure, and neither a product nor load cause is
claimed. Muse owns failure-transcript capture before another primary matrix.
Log: primary-v013-release-gate.log on the build volume. v0.1.3 remains unpublished.

Reviewed documentation corrections distinguish the transient remembered Custom
slot from the persisted active draft, describe nondecreasing capture times and
view-adapter grouping, and clarify that an oversized physical record stays
standalone rather than being constrained to 64 KiB. Duplicate active TODO rows
were removed; five distinct feature/fix assignments remain alongside release
validation. These changes do not alter the tested runtime.

The supervisor recovered the user's source-discovery questions. Docker log
candidates, Linux tee/open-writable-file/stdout/stderr discovery, exclusion of
SQLite/lock artifacts, source-specific assistance context and ANSI presentation
cleanup are present unchanged from immutable v0.1.2. Source assistance still
returns one reviewed SourceDefinition per request; multiple-source proposals
are unsupported. No universal discovery-relevance guarantee is claimed.
A process inspection found a still-running interactive preview-053 process, its
expression helper, old agent waiters and two orphaned test processes. Deleted
preview files do not update or terminate an already-running process; no process
was stopped during this read-only inspection.

## 2026-09-09 — source-assistance expansion and supervisor handoff

The user explicitly requested multiple sources from assistance, stale-process
cleanup, then a new Astra Implementer to take over supervision. Multiple-source
proposal/review/admission is now an active task; the current single-definition
prompt and parser are not claimed to support it.

Cleanup targeted 72 identified stale processes: 33 obsolete Claude shell jobs
and their descendants, two orphaned test processes, the deleted preview-053
interactive process and its expression helper. PID ownership and process start
times were rechecked before signalling; no identified live survivor remained.
Obsolete shell jobs were terminated before their blocked child commands could
resume historical edits. No capture, source, proof or repository file was deleted.
The exact snapshot and signal report are stale-process-plan.json and
stale-process-cleanup.json in primary-sol-scratch on the build volume.

Muse returned test-only commit 655d1a1 adding opt-in bounded failure artifacts
to the multiline PTY test without changing assertions/timeouts. It is not yet
integrated. The next supervisor owns review/integration and another primary
matrix with LVU_PTY_ARTIFACT_DIR set; v0.1.3 remains unpublished.


## 2026-09-09 — Astra supervision, review and primary release matrix

Primary supervision transferred to agent a536fad7-1b2b-4cce-b40f-c2dfbf72bad7
on integration/sol-continuation-20260908, starting at clean e45c781. No main
reset or merge. Muse multi-source implementation is assigned to
5b39ecc4-e3da-455a-a95d-492221208f03 in lvu-muse-multi-source; existing Muse
workers retain their independent paths. The proposed batch is bounded to eight
reviewed definitions, with single-source compatibility and explicit in-product
Apply before admission. This is work in progress, not integrated support.

Reviewed test-only 655d1a1 is integrated as ff9107a: optional failure-only multiline
screen/error evidence and a 64 KiB terminal tail survive fixture disposal without
changing assertions or deadlines. Fields test ffdda5c is integrated as 94ddae1:
closing must remove the dialog, restore base rows and allow Details to open.
It covers empty and structured events. Worker py_compile and eight actual PTY
runs passed against immutable v0.1.2; the hypothesized pending-row transient was
not reproduced. Primary queued the full matrix on the unchanged v0.1.3 runtime,
with evidence under primary-astra-scratch/v013-matrix.log and failure artifacts
under primary-astra-scratch/v013-matrix-artifacts on the build volume.

Viewport 73456b1 has a reproduced fail-without-fix byte-cap regression and green
live/TestBackend/clippy worker evidence. Retaining the overlap/identity test and
actual viewport PTY were requested before integration into the next runtime.
It does not establish a full 512 MB capture or soak fix. Autosave d1b26a6 remains
unaccepted: a late bookmark failure can commit a changed view while reporting
failure, and failed/conflicting views can overwrite source metadata. Per-item
rollback and regression tests were requested. Journal 8a1a043 remains unaccepted:
review requested removal of a scheduling-sensitive observation oracle, overdue
commit priority during continuous pages, and preservation of event-channel-close
cleanup. None of these candidates is part of the v0.1.3 acceptance runtime.


## 2026-09-09 — v0.1.3 primary acceptance complete

Primary matrix on 1481ef6 passed **73/73 in 133 seconds** (four workers), after
integrating only the two reviewed PTY changes and documentation. Runtime remains
9176e6b: application SHA-256
d0afea60727bc887e0dfe1f8386525ecf110d26ef541b04405231bf1acf3893d.
The inherited primary formatting, complete workspace Rust tests, workspace
all-target clippy, bridge typecheck/tests/build and app/demo builds passed at
9176e6b; matrix task confirmed app/demo builds fresh. Python compilation of both
changed PTYs passed. Validation held sol-validation.lock and used a marked
private /tmp fixture root, now removed; logs and optional failure-artifact root
remain on the volume under primary-astra-scratch. No failure artifacts were
produced. An independently launched multi-source compile briefly overlapped
this matrix despite lock coordination; the worker was told to complete the
active command and put all subsequent costly gates inside one locked script.

The earlier primary 72/73 multiline timeout remains unexplained. Three isolated
passes and this full matrix establish successful reruns, not a root-cause fix.
The full volume-backed soak remains unaccepted, including preserved cold-query
and autosave-flush shutdown failures. No general capture or leak-resolution
claim is made.

CHANGELOG now names v0.1.3 and explains multiline collapse/Enter expansion and
optional repeated-event folding separately. Primary is cutting the matching
immutable annotated tag. Muse distribution owner
9b0f0727-bdab-47df-ade7-faecc23732e5 owns four-archive/hash/resource checks and
exclusive /tmp/tap11 formula publication after this accepted tag is sent;
primary retains installed-archive PTY acceptance. v0.1.2 is still the public and
installed runtime until archive checks and publication finish.


## 2026-09-09 — user corrects grouping semantics; publication held

Before tag creation/push, the user clarified that grouping must identify true
log-event starts through a configurable rule, potentially an enrichment column
being non-null, and collapse every record between successive starts. The
integrated Auto/continuation heuristics do not satisfy that requirement, despite
their green tests. v0.1.3 was not tagged or published; distribution acknowledged
the hold and verified remote tag/release absence. The source version remains
0.1.3 in preparation, with v0.1.2 still public and installed.

W22 Muse owns the correction: one native Polars boolean start criterion with a
column picker for non-null checks; all intervening records join regardless of
lexical shape. A valid null is evaluated normally by is_not_null, distinct from
pending/failed prerequisite data. Preserve legacy persisted grouping meaning,
last-good fencing, stable identities, live raw visibility and bounded paging.
Large events must not acquire invented starts merely from the heuristic
64-record/64-KiB limits. Query expression-seam ownership transfers from the
finishing diagnostic worker to W22, while app memory.rs remains autosave-owned
and ui.rs status remains viewport-owned. Primary retains shared documentation.

The diagnostic candidate is now 4e066ce plus 1a96a76, with batch-scoped missing
column wording and later-stage ordering diagnostics; targeted view/PTY evidence
is still pending. Journal candidate 443fdae needs its expired-commit regression
to establish an explicitly overdue initial state, replacing the remaining
100,000-page hardware-speed assumption. Neither candidate is integrated yet.


The user then explicitly confirmed implementation of their version *on top of
existing enrichment*. Final scope is selection of an accepted enrichment output
column: non-null starts an event, null continues it. Recognition logic belongs
in the enrichment editor; Grouping does not need a second expression editor.
W22 is authorized to implement immediately through the existing typed projection,
with explicit failed/pending handling, and normal grouping UX replacing heuristic
recognition. Legacy persisted settings retain their meaning. app memory.rs is
excluded from W22 ownership; existing persistence plus separate tests suffice.


## 2026-09-09 — unified grouping and enrichment-only pattern ownership

The user clarified that runs of equal values and event-start/filter grouping
are modes of one grouping feature, both consuming enrichment outputs. Patterns
and extracted keys belong in enrichment, with raw-text search as a convenience
exception. W22 ownership expands to the existing folding engine/component and
scoped grouping app methods. Normal new grouping should expose Run/Filter/Off;
legacy saved semantics must remain explicit and stable.

Primary code audit found current exceptions: folding's built-in pattern_key
normalizer; live timestamp recognition and severity aliases; direct colour-rule
predicates; correlation's raw JSON/logfmt-only key extraction. Basic structured
input decoding, capture metadata and raw search were identified separately as
boundary choices. These other consumers are recorded as unresolved follow-up,
not claimed migrated. Run keys currently truncate to maximum_key_chars before
comparison; distinct long values can merge. The corrected grouping must preserve
exact key semantics rather than use truncated presentation strings as identity.


## 2026-09-09 — live union views authorized

The user requested implementation of a union derived view over existing views
from different sources, merged in timestamp order, then reusable with ordinary
filters and grouping. It is now Working in TODO. This supersedes a need for
special-purpose correlation extraction as the primary workflow; existing
correlation is not silently removed. A union references captured records rather
than acquiring them again. Stable identity, deterministic time ties/missing-time
handling, overlapping inputs, live refresh and dependency revision fencing are
required design boundaries. Primary is assigning a Muse worktree for this
bounded feature alongside grouping and multi-source assistance.


User requested explicit tracking of all related items. TODO now separates
AI-proposed UTC timestamp/severity enrichment columns and view-role selection,
colour classification through enrichment, and enrichment-key filtering over
union views as the correlation replacement. They remain Open until assigned;
recording them does not claim implementation. Existing multi-source assistance,
unified grouping and the four independent reliability fixes remain active.


## 2026-09-09 — independent Sol review assigned

At the user's suggestion, primary launched a dedicated reviewer using the
configured Sol Implementer profile in lvu-sol-review (agent
528a1517-fa4f-4075-b43c-e7ce65773eb6). Review starts with autosave
0d3d828 and journal b4c77f5, then viewport90e802c and diagnostic/test revisions.
The reviewer checks code and evidence independently and reports concrete findings;
Muse implementers retain their paths, and primary retains acceptance/integration.
No new review builds are authorized by default. The latest enrichment PTY failure
was traced to a wrapped diagnostic wait and cleanup typing q into the still-open
editor, not established load-induced shutdown failure; e2fc618 addresses the test.


Current union implementer: 397504aa-a443-4491-b412-d72f44b43bc1 in
lvu-muse-union-views. Enrichment diagnostic worker has also been assigned a
read-only seam/design pass for the UTC timestamp/severity assistance shortcuts,
without editing paths currently owned by grouping or source assistance.
Diagnostic final PTY commit 2ccb9c2 passed under the shared lock with the same
timeouts, including actual draft editing and live append. The intermediate
whitespace-normalized wait still failed because terminal borders interrupted
the phrase; row-contiguous token checks were verified against saved failure
artifacts before the successful run. No load-causation claim remains.


Independent Sol review of autosave0d3d828 found a compile error at memory.rs450
(double dereference of latest:&usize). New8929174 corrects that one line; prior
candidate must not inherit final revision's validation. No second verified
atomicity/ack-order defect was found by read-only review. Final worker gates are
still required; no build/slow-volume evidence is implied by the review.
Report: sol-review-scratch/autosave-0d3d828-review.md on the build volume.


UTC timestamp/severity AI shortcuts and explicit column-role implementation
are now assigned to a8f5e7f on a new feature branch, preserving the diagnostic
candidate ref. Role defaults are unset; role assignment follows accepted reviewed
enrichment, with selected event time distinct from capture metadata. Normal new
flows must not retain hidden timestamp/severity inference. Existing saved legacy
semantics require explicit compatibility handling. Source/grouping/autosave
owners were contacted about narrow disjoint UI/projection/persistence seams.


## 2026-09-09 — viewport byte-cap correction integrated

Primary reviewed and cherry-picked90e802c (reverse insertion retains a drawable
prefix; status names loading over indexed rows). Worker exact-final evidence:
live27/27 including retained overlap/order/identity regression, TestBackend2/2,
clippy/fmt clean; old insertion control fails the byte-cap regression. Actual
blank-view PTY passed on the worker's fresh final binary with restart/restored
accepted-filter behavior. Logs: sol-muse-viewport/{finish-90e802c.log,
pty-blank-view2.log,gate-73456b1.log}. Primary integrated Rust/PTY validation is
pending; independent Sol review continues. No full512MB or volume-soak pass is
claimed and the overall viewport TODO remains unresolved.


Independent Sol review blocks journal b4c77f5: continuous queued pages can prevent
blocking_wait from observing event-channel closure, delaying Incomplete/join and
lease release indefinitely. Also a cancelled caller drops its page-gate permit
while the request can remain queued or active, breaking the claimed one-request
bound. Worker must detect closed/drained events before paging, preserve buffered
Finish ordering, and transfer an owned permit into PageRequest until service/drop.
No journal change is integrated. Evidence corrected:60 ingest passes plus one
ignored measurement on ba1c028; final b4c77f5 has eight lib passes and clean clippy
after the behavior-neutral config->_config rename. Report is
sol-review-scratch/journal-b4c77f5-review.md on the volume.


Viewport independent Sol review90e802c is READY with no findings for the bounded
byte-cap claim. Primary gate at18392c6 passed live27/27, viewport_loading2/2,
clippy(lvu/live all-targets), app build and actual blank-view PTY. App SHA-256:
cfff82c7c49d1846a36c1e9de42b1753e478a6620838d1374fb9d4858847dc50.
Log: primary-astra-scratch/viewport-integrated-gate.log; private marked fixture
root removed on exit. Full release matrix will run on the next complete accepted
runtime; this focused gate does not claim512MB/RSS/soak/general-thrash coverage.


## 2026-09-09 — active feature seams and build isolation incident

Column-role candidate ae4eea2 is a worker-only scaffold: prompts, palette entries
and unset role fields passed focused tests, but display/persistence wiring is
not complete or integrated. Narrow persistence and accepted-membership role
hooks are assigned; grouping and union owners coordinated disjoint paths.
Union candidate 82ccf03 now consumes frozen accepted values rather than truncated
display projections; executable registration, publication and UI remain pending.

W22 reports recent bare mise Cargo checks wrote to the primary target. Current
primary artifacts must be treated as suspect regardless of freshness timestamps.
Before the next acceptance gate, rebuild affected workspace path crates under
the lock with an explicit primary target; preserve external dependency caches,
proof and captures. Prior exact-binary gate results remain historical evidence,
not proof of the current target contents. Installed v0.1.2 is unchanged.
Autosave's current dcd83ff run has a misleading primary-target environment probe,
but /proc confirms explicit --target-dir and rustc output paths use its private
target. Its current script lacks the claimed marked fixture and reliable outer
exit propagation; read the recorded GATE_EXIT and test outcomes directly. These
script shortcomings have been returned to the implementer for correction.


## 2026-09-09 — missing-column diagnostics integrated

Integrated reviewed stack 4e066ce, 1d3a2cb, e2fc618 and 2ccb9c2 as one bounded
deliverable. Independent Sol review is READY with no findings. Worker query
45/45 and view3/3, clippy and app-build evidence remain valid for unchanged
runtime. Final actual PTY passed with the same timeouts, real draft edit,
last-good rows and live append; the durable locked rerun is
lvu-muse-enrichment-logs/pty-rerun-2ccb9c2.log (PTY-EXIT:0). Historical cleanup
and wrapped-matcher failures remain preserved and are not load-flake evidence.
Primary focused integrated validation follows a rebuild of workspace artifacts
affected by target sharing. This diagnostic change is not yet published.

Primary integrated diagnostic gate at 8b114b3 passed after package-scoped cleanup
of primary workspace artifacts under the shared lock: fmt, query45/45,
missing_filter_column3/3 and query/view all-target clippy. Exact log and script:
primary-astra-scratch/enrichment-integrated-gate.{log,py}. Inner mise environment
explicitly set the primary target and metadata confirmed it. Marked private
fixtures were removed on exit. No app binary or full matrix was built by this
gate; the next runtime gate must build the app before any primary PTY run.


## 2026-09-09 — reviewed autosave batch integrated

Integrated 135ceac after Sol source review of dcd83ff and primary verification
that the final delta only removes seven dead stash assignments immediately
before return. Exact worker gate passed memory batch6/6, app memory20/20 and
app/memory all-target clippy, with GATE_EXIT:0. The corrected script is retained
as lvu-muse-autosave-gate-script.sh; log lvu-muse-autosave-gate.log, final run
08:04:52Z. It uses explicit target arguments and inner mise environment, marked
private fixtures, exit-preserving cleanup and the shared validation lock.
Primary integrated validation follows. No changed deadline, durability rule,
fsync-causation or full slow-volume-soak resolution is claimed.

Recovered worker completion reports also identify journal45c0e3b and source
c324530 for immediate delta review. Roles and grouping were stalled at
coordination despite approved scopes; explicit implementation continuation
has been sent, with union application wiring assigned to its existing owner.

Primary autosave gate at d6efbd3 passed batch6/6, app memory21/21, formatting
and app/memory all-target clippy. Log and exact script are retained at
primary-astra-scratch/autosave-integrated-gate.{log,py}; shared lock released
and marked fixtures removed. No full slow-volume acceptance is implied.


## 2026-09-09 — journal scheduling and fallback bounds integrated

Integrated 9fdb975. Sol reviewed the 5f93d01 runtime without findings after
closure, queued-request permit and direct-fallback permit corrections. Primary
checked the final test-only delta: abort is awaited, the private gate remains
unavailable while detached I/O is parked, and owned-channel drop releases the
probe on failure. Exact final focused test, clippy and fmt pass; identical
product tree 5f93d01 passed 48 tests (lib10/runtime37/scheduling1). Earlier
45c0e3b full-suite evidence is 61 pass/1 ignored, not an exact-final full-suite
claim. Logs are retained in lvu-muse-journal-gate.log. Primary integrated
ingest validation follows. No 12.921-second causal explanation, performance
improvement or full soak acceptance is claimed.

Primary journal gate at fd65403 passed 48 tests (lib10/runtime37/scheduling1),
formatting and ingest all-target clippy. Exact log/script retained as
primary-astra-scratch/journal-integrated-gate.{log,py}; shared lock released,
marked fixtures removed. Final independent Sol review of 9fdb975 is READY
without findings, including the deterministic detached-read guard assertion.


## 2026-09-09 — multiple reviewed sources integrated

Integrated 7ebb649. Independent review of c324530 cleared all prior findings
except same-ID stdin retention; primary verified the final amendment routes
that arm through the shared policy comparison and the focused test passes.
Prior worker gate gate-20260909T085408Z.log passed bridge105, app44, sourceUI14,
other source UI2+3, clippy, app build, existing single and new multi-source
PTYs (wide/narrow, partial failure, repeat confirmation). Final amendment adds
one passing stdin comparison test; no other runtime paths changed.

The delivered flow has bounded plural proposals and legacy single answers,
explicit Applying state, cancelled-review authority removal, pending-definition
comparison and typed lossless policy/path comparison. Unresolved path spellings
remain exact; only two canonical paths or two identical unresolved spellings
compare equal. Canonicalization uses a short-lived per-pass cache and retains
filesystem race limitations; no hardlink/bind-mount identity claim is made.
Primary integrated Rust/bridge/UI/PTY gate follows. Not yet published.

Primary integrated multi-source gate at 0dc9acf passed bridge105, app source
checks, source TestBackend suites, formatting, app/lvu all-target clippy and
a fresh app build. Existing single-source PTY passed at 140x28 and 54x16;
new multi-source PTY passed batch review, explicit Apply/repeat-confirmation
and partial failure. Exact app SHA-256:
1b89ba6caae637e5f000e9b6aca3b9c1a7bac58fe1643a7145cbf16c20f19b76.
Log/script: primary-astra-scratch/source-integrated-gate.{log,py}; marked
fixtures cleaned and shared lock released. Concurrent primary changes were
documentation only (50c3308). Full release matrix awaits corrected grouping.


## 2026-09-09 — remaining feature review boundaries

Grouping68fca6d remains worker-only: normal Folding still exposes message
normalization despite the unified Run/Filter requirement; key encoding must
respect bounds before allocating and native float equality. The owner is
correcting all normal entry points and exact-key tests before release review.

Roles f860203 remains worker-only. Independent review found pending role intent
left armed on a fork origin, projected null/error markers treated as usable
values, missing display-zone conversion and duplicate timestamp shortcuts.
The revised gate must identify its source tree and binary exactly; prior roles6
metadata does not prove the exact f860203 tree. Fixes are assigned together.

Union d327ddb remains worker-only pending exact live-input publication fences,
cumulative pre-allocation budgets, caller-generation completions and strict
dtype evidence. Its application UI and downstream query wiring also remain
in progress. None of these candidates is published or counted as accepted.


## 2026-09-09 — grouping release review and final gate preparation

Independent review of d2330a6 identified retained-state accounting defects:
projections dropped beyond the stored group page were still charged, while
new group base state and run-key capacity were undercounted. Unsupported
valid key dtypes also became pending singletons instead of rejecting the
candidate and preserving last-good grouping. Both corrections and bounded
regressions are assigned with the normal Run/Filter/Off routing correction.
The candidate remains unaccepted. Narrow expansion must expose member text
rather than consume the visible width with full identities.

Primary prepared v013-final-gate.py in primary-astra-scratch: it binds the
clean source SHA, confirms the primary target, runs workspace Rust and bridge
checks, builds both executables, records hashes and runs the full PTY matrix
with durable failure artifacts. It has only been syntax-checked, not run.
The prepared archive acceptance additionally covers Run/Filter and multiple
reviewed sources. Roles and union remain outside the pending v0.1.3 scope;
distribution preparation proceeds while grouping is corrected. No new tag
or release has been published.

The prepared final gate was corrected after independent script review: separate
build tasks, explicit bridge installation, a fresh commit-and-UUID target,
and unique evidence with exclusive acceptance records. The record includes
source, version, target, script hash, log path/hash and both binary hashes;
source cleanliness and binary stability are checked again after the matrix.
Sol static delta review is READY for script SHA-256
b0f2f0158172f169e7d27b99cab99ae1467af4f33cb00ddfb74ac5c173a5093c.
This is script review only, not release acceptance. The installer is separately
being corrected to validate staging, authenticate archive provenance and run
PTYs outside the checkout before version promotion.

Installer static delta review is READY at SHA-256
e62609379e9870ca9e60b6ed206a27fcdcc2260ee22eda1dc08657d445292ee9.
The script fences the checkout, authenticates gate/tag/workflow/archive evidence,
validates staging outside the checkout, and preserves retryability on promotion
failure. Control-flow probes cover cleanup after rename and missing/corrupted
binaries; no actual v0.1.3 archive has been installed.

Grouping ab63a7b engine delta is source-reviewed READY; b74a150 routing is
sound apart from the retained collapse-all action and required-command list,
which are assigned with final UI tests. Worker-only roles451c182 has been
submitted for independent review; union app-flow work continues. These later
features remain outside v0.1.3.


## 2026-09-09 — unified grouping integrated for final acceptance

Integrated the reviewed delta 655d1a1..be577da. Run/Filter/Off now shares normal
keyboard, palette, Fields and summary routing. Retained group accounting and
unsupported-key rejection preserve bounds and last-good publication. Independent
engine and routing delta reviews are clear. The worker's last gate was interrupted
by a supervisor follow-up during compilation; it is not final acceptance.

Primary corrected the large-view PTY to sample blank frames from dialog close
through grouping and ungrouping completion, rather than only after completion.
The exact clean integration commit will now run the reviewed isolated final gate.
No v0.1.3 tag or archive acceptance is claimed by this entry. Roles and union
remain separate work in progress outside this release.

The first isolated gate at 0e9ab4c caught an unknown Run-version parser fallthrough.
The reserved Run/Filter namespaces now reject unsupported versions before legacy
regex handling; independent review cleared the fix. Workspace tests then exposed
stale legacy-route assertions and an index-lock fixture acquisition race. Tests
now await Grouping completion, explicitly enable restored legacy fixtures, and
acquire the fixture index lock before measuring the unchanged retry window.
The complete repair workspace run passed (grouping-repair-gate-3.log), followed
by workspace clippy and bridge105 (grouping-repair-ui-gate.log).

The first integrated PTY matrix passed 69/75. Preserved focused transcripts show
stale singular-source request, retired folding labels, text-first expansion,
uncollapsed selection, and hidden narrow-status expectations. Corrected stories
use accepted enrichment keys and actual grouping counts. All six failed suites
now pass focused reruns: repair2 real-source/Run, repair3 field-exploration/large
grouping, repair4 Fields mnemonics, repair5 summary. The large 60k fixture samples
through transitions and verifies count restoration; no volume-soak claim follows.
Sol independently cleared the test repairs. Matrix failure output is now printed
in full into the durable gate log. A fresh exact-commit final gate follows.


## 2026-09-09 — v0.1.3 published and archive activated

Exact source `8ca75d0074aa85964f5d493d88d1be3a48fcc0f4` passed the isolated
final gate: 1349 Rust tests passed (7 ignored), workspace clippy clean, bridge
105/105, and PTY matrix 75/75. Preserved acceptance is
`primary-astra-scratch/release-gate-8ca75d0074aa-6f4ad43dbd68449f8ba75d866ea174f2/accepted.json`
on the build volume. Its source, script, log and preserved binary hashes were
independently reviewed consistent. Earlier failed gates remain preserved.

Annotated immutable tag `v0.1.3` has object
`4a0cac74c1af0c7fb38397ae2a9dea5b2f5fba2c` and peels to that source. GitHub run
34411091946 passed all four native jobs and publish. Distribution checks passed
73/73; archive files match SHA256SUMS and API digest/size. Tap `67359d3` was pushed
after exact-tag rendering, style and strict audit.

The installer at SHA256
`9f3442e6bc682cff4f769c3cba9172cbec555ae61792f285fc7f52f377e99603`
passed independent review after draft lookup was rebound to both release ID and
tag. Under the shared lock it verified provenance, exact clean source, bundled
helper/bridge, full-tree manifest and six actual PTY suites from outside the
checkout. It activated `versions/v0.1.3`; records are
`versions/records/v0.1.3.json` and `v0.1.3.tree.json`. Installer log:
`muse-v013-distribution/v013-delivery/installer-run.log` on the build volume.
The install record correctly records draft=true at installation time; publication
followed at 22:58:15 UTC, release ID 385891552. Primary independently verified
public isDraft=false and `versions/latest` => `v0.1.3`.

The full volume soak remains unaccepted. Roles candidate `ae449fd` is reviewed
READY but not integrated; union remains in progress. Neither is in v0.1.3.


## 2026-09-09 — column roles integrated for acceptance

Applied independently reviewed `ae449fdebf23d0c890fca74b6ee8d3dfa6b2c079`
on top of the released grouping source. The Fields merge preserves both role
assignment and the unified Run grouping action. Severity consumes only a ready
canonical enrichment value. Timestamp rendering uses structural declaration and
readiness markers plus the accepted basis nanoseconds and display-zone formatter.
Removed outputs cannot silently regain authority through same-named raw fields.
Assistance shortcut intent follows accepted candidate ownership and persistence.

Worker evidence and review are preserved under `roles-ae449fd-*` and
`roles-ae449fd-delta-review.md` on the build volume. The integrated tree still
requires its own Rust, bridge and full PTY acceptance; no release includes it yet.


Column roles integrated acceptance completed at
`06bd07ae7f078122ab8f3242b834ab4314969111`. Runtime source is unchanged from
`88cc6e1`, which passed 1386 Rust tests (7 ignored), workspace clippy, and bridge
105/105. Its first matrix passed 75/77; the preserved transcripts identify stale
Ask-title and implicit INFO readiness expectations. Two PTY-only repairs retain
workflow assertions and prevent teardown from masking the save-feedback failure.
Both focused reruns and the full matrix passed 77/77 with unchanged binary hashes.
Evidence: `primary-astra-scratch/integration-gate-88cc6e1ad417-9353eddd957e4a75a0de93d9cb646fe5/commands.log`
and `integration-gate-06bd07ae7f07-0dda6d10b0b346da825c103d7d395f8e/accepted.json`.

Prior integrated test failures and the one test-helper compile correction are
preserved in dc483d1/f9b3e18/0db5150 gate logs. Sol cleared the resolved Fields and
group projection seams and the Rust fixture repairs. Column roles are Ready for
the next release; they are not part of the published v0.1.3 archive.


## 2026-09-09 — parallel backlog ownership and v0.1.4 preparation

Version v0.1.4 is prepared for the accepted enrichment column roles; v0.1.3
remains installed and public until the next archive is accepted. The app crate
and lock version agree. A fresh exact-source release gate follows.

Remaining TODO ownership is active: union397504aa; viewport/512MB lead8b525718;
autosave0523e863; journal/cold-queryfc42914a; enrichment-based coloura8f5e7f1;
shared-capture architecture5b39ecc4; named zones12c3583a; enrichment/union
correlationc98fb965; distribution/platform evidence9b0f0727. Sol528a1517 reviews
candidates independently. The three storage investigations share one lead soak
rather than running competing volume benchmarks. Builds and heavy tests remain
serialized under the validation lock while design, implementation and review
proceed in parallel. No new backlog completion is claimed.


## 2026-09-10 — v0.1.4 published and archive activated

Exact source `984b6f0dda96c782de3fb7c3e9a9bf169db08eae` passed the isolated
release gate: 1386 Rust tests passed (7 ignored), formatting and workspace
clippy clean, bridge 105/105, and PTY matrix 77/77. Both binary hashes were
checked before and after the matrix. Producer SHA256 is
`4d1cb5e5ee256fb5317ce79bb6da38799e9384dd79949e9a0ed444f22979ae39`.
Evidence is under build-volume
`primary-astra-scratch/release-gate-984b6f0dda96-977250d2a08642b38f8e6b09b38c1bd6/`.

Immutable annotated tag object `ad6664675f8659f3a89b64f38a54c1c3f74c728e`
peels to that source; workflow 34418420557 passed all four native archive jobs
and publish. Release 385921636 became public at 00:20:58 UTC. Primary checked
public release state independently after installation.

Reviewed installer SHA256
`f14e3c912269bb488f78b5d6199fb3d0162db47b32cbf23224a1bf553c7a07e9`
ran under the shared lock with the pinned producer hash. It verified official
assets, exact clean source, full archive layout/tree, bundled helper/bridge,
and eight mandatory actual PTY suites from a disposable external cwd. The
archive acceptance includes column-role and shortcut-role workflows. It verified
the released launcher after promotion and wrote exclusive records at
`versions/records/v0.1.4.json` and `v0.1.4.tree.json`. The record correctly
captures draft=true before subsequent publication. Installer log:
`muse-v014-prep/v014-delivery/installer-run.log` on the build volume.

Primary verified `versions/latest` points to `v0.1.4` and its binary SHA256
`10b2f8e9c3db925698a1344c06e6edb6e818424a65b1ec0b5b1f84d6973adbd2`
matches the acceptance record. The full volume soak remains unaccepted; no new
cold-query, shutdown durability, or managed-memory performance claim follows.
Named zones, union/shared-key composition, colour classification, shared capture
and platform validation remain separate assignments.


## 2026-09-10 — named zones integrated for v0.1.5 acceptance

Applied the reviewed `9fc7e53..4b9d3b7b2583c1a04cdff70047a0a4ab13da580e`
runtime/test delta onto the v0.1.4 publication documentation. The app and lock
versions are now 0.1.5. Worker evidence: lvu 49/49, app settings 16/16, both
crates all-target clippy, app build and real role PTY including Berlin winter,
summer, invalid repair, save and restart. Final log SHA256:
`5d30e583050cbbc1c481742a308fea61bb9cd2de93e62d57af574fb03ea6b167`, under
`sol-named-timezones-4b9d3b7/final-gate.log` on the build volume.

The earlier 8bf6551 restart failure remains preserved; saved settings were
correct but startup did not apply the zone to Appearance. The final configuration
assignment and regression are independently reviewed READY in
`sol-review-scratch/named-zone-4b9d3b7-delta-review.md`. The full integrated
release gate and archive validation are still required; no v0.1.5 tag or
publication is claimed here. The installed release remains v0.1.4.


## 2026-09-10 — statistics throughput measurement boundary repaired

The first v0.1.5 gate at `22dc294` failed the unchanged 200,000 records/CPU-second
floor: 150,005 records, 0.376 s wall, 0.780 s process CPU, 192,314 records/CPU-s.
An exact-binary rerun passed at 319,157 records/CPU-s with similar wall time.
These observations support measurement contamination, but do not directly prove
which concurrent work contributed. Both logs remain preserved. The subsequent
remaining checks passed clippy, bridge checks, builds and all 77 PTY suites;
those partial results did not produce release acceptance.

Integrated reviewed `c339d46b3cf8a9675f40fe2817379cf26a5e1d6f`: the test now
requires terminal durable capture and exact live-index count/high-watermark
equality, joins the raw index workers, then measures the same production journal
statistics path. A fallible process CPU utility uses getrusage instead of an
assumed procfs tick rate. The correctness assertions and throughput floor remain
unchanged. Focused validation passed the CPU utility test and statistics test
(150,004 records, 0.341 s wall, 0.435 s CPU, 344,842 records/CPU-s), followed by
all-target clippy for lvu-core and lvu-view. Independent static review found no
issues. Evidence: `sol-stats-throughput-c339d46/focused-validation.log` and
`sol-runtime-review-scratch/stats-throughput-c339d46-review.md` on the build
volume. A fresh full release gate remains required.


## 2026-09-10 — v0.1.5 published and installed

Exact source `2a636c02eb27ecb25597cbfb99fe3c403aad760b` passed the fresh full
release gate: 1392 Rust tests passed (7 ignored), clippy and bridge checks clean,
and PTY matrix 77/77. The accepted record SHA256 is
`ac6fefd0adc2daa37f284ccfd396bd06ff66d73709a666b82c639ac080627c6f`;
commands log SHA256 is
`e7b8a0269cbb4dded0df21a8ab052be08708c6aad20e34e2f796418c64ef2a58`.
Evidence remains under build-volume
`primary-astra-scratch/release-gate-2a636c02eb27-825466a738614d7e9e5bba7816de9bb1/`.
The failed 22dc294 throughput gate and its partial reruns remain preserved;
this acceptance does not establish their underlying cause.

Reviewed producer SHA256 is
`0b618319776c7320f3cc360031dff66ff2315ace8ee175b81f9d27371e2081d5`.
Gate app SHA256 `bf63a763c3b99a4f574b9c9a74fdc0fd756fd708138e29eb002c6e1c2402ac6e`
and TUI SHA256 `18b5be3eac5e17c96502de205e62b5bc270721ec99bb9655928449278f2b07a9`
match the accepted record. Independent consistency review approved the record.
The host was not exclusively idle during all PTYs; a separately observed
diagnostics invocation is recorded in the preserved coordination note. No
exclusive-idleness claim or causation claim follows from passing tests.

Annotated immutable tag object `7c6f3706a3c891aadfdb79fdd20fd9cf32a406e0`
peels to that source. Workflow 34425230184 passed all four native archive jobs
and publish. Release 385951844 became public at 01:51:54 UTC. Homebrew tap
`078724dc1e4e902853d7279121f5986b003babca` was pushed and verified.

Reviewed installer SHA256
`986ff64a888335a490a58e23c2492d87e5c3fb89e2e6608801635b507ce90fcf`
ran under the shared lock with the pinned producer and accepted record. It
verified official archive checksums, exact clean source, bundled helper/bridge,
full-tree identity and eight mandatory actual PTY suites outside the checkout,
including column-role, shortcut-role and named-zone restart workflows. It
activated `versions/v0.1.5`, verified the released launcher and preserved
exclusive `versions/records/v0.1.5.json` and `v0.1.5.tree.json`. The record's
draft=true correctly describes acceptance before the subsequent undraft.
Installer log: `muse-v015-prep/v015-delivery/installer-run.log` on the volume.

Primary independently verified public release state, `latest -> v0.1.5`, and
installed binary SHA256
`25b8ff0b6ab9302cc0c0d502a3e7218982a1452141140bc0076385a963fb5d87`
against the record. Public notes initially carried two stale debug-binary hashes;
primary corrected only those hashes from the authenticated record and verified
the readback. Tag and archives were unchanged.

Named zones are now published. The full 512 MiB volume soak remains unaccepted;
union/shared keys, colour classification, shared capture and platform runtime
acceptance remain separate unfinished work. No broader performance, durability,
or platform-support milestone is claimed here.


## 2026-09-10 — custom target ownership cleanup guard integrated

Integrated reviewed `4d6a8e84ec9dfacba3ab5f1780c7aaf7ee4cb168` as
`fa4c69d`. The janitor no longer infers a vanished owner from a custom target
name alone. Bounded Cargo dep-info evidence identifies a unique owner; unknown,
malformed, conflicting or exhausted evidence retains the target. Lazy scoped
scandir enumeration consumes no entry beyond its aggregate cap. Conventional
existing-worktree policy and uniquely evidenced vanished-owner cleanup remain.

The active `lvu-muse-colour-target` exposed the original false positive. Its
Cargo paths contain parent components, which the deliberately strict parser
treats as unknown; retention is the intended outcome. Primary verified the
review and evidence hashes and reran all 14 focused Python tests successfully.
Review: `sol-runtime-review-scratch/janitor-target-ownership-4d6a8e8-review.md`
on the build volume, SHA256
`13dd6bb23d901a3a98942abb16ed18fd35c6919e3e55e521d07bffadeda59c8a`.
Owner validation SHA256:
`ee8ad8cc6fc590ea99a1ac2782a5b6ebb7f0456b26e5811f91bd0072918a7459`.
No Cargo build or cleanup is claimed by this integration.

Primary `mise run janitor:dry` subsequently completed with exit 0 and
`janitor would reclaim 0.00 GB`; it proposed one tiny marked temporary
reproducer, and did not propose the active custom colour target. No cleanup
was run. The scan spent several minutes waiting on volume I/O; other volume
readers remained stalled after it finished. With roughly 4.1 GiB free and
sustained I/O pressure, new local Cargo/PTY/performance gates are held pending
storage recovery and a reviewed disposable-build reclamation plan. This is
host evidence, not a product performance or durability result.

After volume reads recovered, primary executed the reviewed narrow failed-22dc
reclamation plan while holding both the shared validation lock and that target's
Cargo lock. A privileged read-only scan checked process exe/cwd/root/fd/maps
with no live references or unreadable-process errors. Deleted only reproducible
deps entries (excluding the exact statistics executable and sidecar), build,
fingerprint, incremental and temporary artifacts in target
`release-22dc294042d2f6d24aa25cc9cc397b74d8eb3c1f-930f53e2de9b4c30b75aa0b073e6ec3e`.
Pre/post hashes matched for the retained statistics binary
`stats_throughput-997fe4120aa00459` (SHA256
`f6d1061d9469583bb66c0605f52f5377b9a0becb4d418cfe78ccbb90e3ebff54`),
its sidecar, and top-level app/TUI binaries. The installed latest link was
unchanged. Logs, captures, proof, other targets and release records were untouched.
Free space increased by 3,933,835,264 bytes; `mise run disk:check` passed with
8G free. Exact before/after proof: build-volume
`primary-astra-scratch/reclaim-22dc-1789007071385917404.jsonl`, SHA256
`b939daca706dc272e7346b4346390d2cfa2b487d803b44d5a97363591f55689b`.
This cleanup preserves the unresolved methodology evidence; it does not prove
the cause of the earlier throughput variability.


## 2026-09-10 — native platform validation integrated

Merged reviewed candidate `9dff245ddcf0c217116b168fe7262eb47c65bb2e` after
hosted run 34428200565 attempt 1 passed. Actions executed synthetic merge
`5b3ac599eb27fd8db480049c37795cde8f5ce782`, whose parents bind the existing
`c8eda166` baseline and exact candidate. Arm64 Darwin passed all-target compile,
portable staging, installed resources, kernel PTY, terminal restoration, orderly
and injected-failure cleanup. Intel Darwin passed compile only; Windows passed
its exact known-blocker characterization and remains unsupported.

Primary rechecked all 14 retained manifest entries. Independent review also
compared extracted bytes against original ZIP members and verified source,
binary, verdict and owned-process bindings. Review SHA256:
`d814b0b7c282d4a91e11753b7ba0c5c907444e02cce78ea060eb5172feaa98b0`
(`sol-runtime-review-scratch/platform-validation-9dff245-hosted-final-evidence-review.md`).
Artifact manifest SHA256:
`02ecf666e826a50807a12aecd9285ebbdb39b59e0c4b343544fa246668458ebb`.
Artifacts remain at build-volume `platform-validation-hosted-34428200565-attempt-1`.
Prior failed runs and the superseded self-referencing local manifest remain
preserved. No new release or human macOS/Windows runtime acceptance is claimed.


## 2026-09-10 — live unions and shared keys integrated for acceptance

Merged `3a32a6c641d8d0a5de31ddb7d78908af2b149c4f` as `044daeb`, preserving
the accepted `2a636c0` baseline. Final bounded worker review found no remaining
defect in transient retry, pre-retention memory charging, source publication
or accepted shared-key authority. Independent persistence review passed
transactional restore, role preservation and exact-key/grouping round trips.
Reports remain in `sol-union-review-scratch/union-3a32a6c-final-bounded-review.md`
and the review conversations.

Owner validation passed the transactional restore test, all 15 union-worker
tests and targeted clippy. Initial `606232a` worker-test compilation failed on
a missing `FrozenInputLimits` import; `3a32a6c` changes only that test import.
Both logs remain under `lvu-muse-union-logs/targeted-*-20260910.log` on the volume.
Actual UI validation is still running; no release acceptance is claimed yet.
Long-running test infrastructure work is paused and preserved at `581b780`
while feature integration takes priority.

## 2026-09-10 — enrichment colour classification integrated

Merged colour candidate `e5d0038` as `5a04c00`. Its final test amendment is
byte-equivalent to independently diagnosed `8314aa7`: the 80-column status
fitter intentionally omits `enrich:on`, and the applied dialog reports two
rules rather than the stale singular assertion. The corrected combined FOLLOW
workflow observes published Column/Value inventory and the exact applied
two-rule state. The focused actual PTY passed in 3.9 seconds against preserved
`b405582` binary SHA256
`2ae75229c943ee7592bd6272f3209997f41422aab388fa27708258cd233b24b7`.
Evidence: build-volume
`lvu-sol-enrichment-correlation-colour-fix-pty-r2.log`.

The earlier apparent colour pipeline stall was a test assertion failure.
Independent inspection also confirmed the accepted-output authority and float
canonicalization fixes. This is not combined release acceptance: the integrated
union worker still needs the new colour query field and actual union colour
constraint handling. That seam is assigned to the colour integration owner;
the union owner separately investigates shutdown timeouts in actual union PTYs.
No full matrix or new release is claimed by this merge.

Primary prepared the app and lockfile version `0.1.6`; locked offline Cargo
metadata agrees. The new gate producer differs from the preserved v0.1.5
producer only in its expected version. The proposed installer adds the two
union PTYs to the eight existing mandatory suites and retains explicit
producer-hash authentication. Independent tooling review passed with no findings
(`sol-runtime-review-scratch/v016-release-tool-delta-review.md`, SHA256
`bd885eb2968688d21fdb15335add22ec991fdfd221919511f355f554480117cf`). No acceptance record or
tag exists for this preparation; union shutdown and colour integration remain
prerequisites to the combined release gate.

The delivery owner subsequently reclaimed only the five approved reproducible
build directories from superseded v0.1.4 gate target
`release-984b6f0dda96c782de3fb7c3e9a9bf169db08eae-977250d2a08642b38f8e6b09b38c1bd6`.
Both validation and Cargo locks were held; a privileged process-reference scan
preceded deletion. Accepted app/TUI hashes matched before and after; installed
v0.1.5, other targets, captures and proof were preserved. The proof records
free space increasing from 7,712,157,696 to 11,602,841,600 bytes. Primary verified
the proof hash and `mise run disk:check` passed at 11G free. Proof:
`primary-astra-scratch/reclaim-984b-1789008971788034110.jsonl`, SHA256
`db29b47a952fcfbfd767ebaefa991c11967a76c63e0401cbdfd0e55f52cf39d0`.
Earlier lock-busy and self-lock-reference refusals performed no deletion.


## 2026-09-10 — restored union input readiness repair integrated

Cherry-picked reviewed `5d9a7cff4dfb5c4ad752eaf3c0d87c0611abd62b` as
`f7a4dc6`. A restored exact-key union can initially see raw input types before
its accepted enrichments publish. Failed first publication now retains the
attempted inputs/filter and retries when input publication changes. Provider
revision affects retry identity only; accepted revision/generation fences remain
unchanged. Unchanged transient failures retain backoff, and identical terminal
source-set rejection remains quiet. Independent immutable review found no defect
in this bounded change.

Owner evidence in `lvu-muse-union-logs/readiness-targeted-5d9a7cff4dfb5c4ad752eaf3c0d87c0611abd62b-20260910.log`
records three passing focused tests (first-publication retry, transient backoff,
source-set rejection) and an app build. Its header also records an untracked
PTY probe; this is targeted evidence, not a clean release gate. The earlier
shutdown-timeout description was premature: cleanup obscured earlier UI
assertion failures. Final union PTYs, colour integration and the full v0.1.6
gate remain pending; no shutdown fix or release acceptance is claimed here.


The owner then completed the full 16-test union-worker suite, targeted clippy,
and both actual union PTYs at `fbcb624`. Consolidated `1d37cbf` has an identical
Git tree; its worker source/tests are byte-identical to the integrated repair.
The remaining two PTY files were adopted from that checkpoint. They select the
actual enriched inputs, verify full restored membership and original stable
IDs, and retain normal shutdown assertions. Both PTYs exited zero, including
UInt64 keys above f64 precision, live append, rejected-candidate rollback,
raw-namesake rejection, slash captures and long-prefix discrimination.
Evidence: `lvu-muse-union-logs/final-both-pty-fbcb624b1e14fc8734564c8d0f252c95a34e50aa-20260910.log`
and `final-rust-fbcb624b1e14fc8734564c8d0f252c95a34e50aa-20260910.log`.
Validated app SHA256: `901e58205e6a52ac97bef6b507ce883c5a63e33abffc60760352501ccd5cbe51`.
The combined primary colour integration and release gate are still pending.


## 2026-09-10 — union colour release verification prepared

Integrated the prepared heterogeneous union colour PTY as `88b24e2` from
`a8b5075`. It uses the native Column/Value controls, ordered overlapping
classifiers, actual terminal attributes, raw-namesake exclusion and restart.
Python compilation and source review passed; runtime execution awaits the
colour amendment and is not yet claimed.

The installer now requires eleven suites, including this new test. Independent
review of consumer `66394e0ce5e3a804b15289257c204e07f5f56529afbbb09ee8c387f00cb828ff`
passed with only a stale count in the header. Primary verified that the final
consumer differs by that one comment line only. Final installer SHA256:
`65db0ead054acd1acc59b860a5fe4c3145cb8179af24fb8dd64b01c481eb2e0c`.
Producer SHA256 remains
`380471d5031c90aa66b2d8fc2fc42c9623923cd758b8d38941f61ca0f357f057`.
Review: `sol-runtime-review-scratch/v016-installer-66394e0-delta-review.md`,
SHA256 `1b2ceb3753964698d108b9e49f8bca7d4ed030dbc9942f8f8833a27200ce84f6`.
These are tooling pins, not an acceptance record or release GO.


## 2026-09-10 — combined union colour source prepared, acceptance blocked

Integrated `77c3c06` and `449a00a` as `48602f1` and `2d0aa98` on the complete
ordinary-colour baseline. The merge preserves both input publication revisions
and colour rules in dependency-attempt identity. Before first publication it
uses the retained attempted colour rules, alongside the retained input/filter,
so rejection/backoff comparisons do not accidentally compare against empty
published rules. This merge hunk still needs integrated validation.

The engine, export and colour-dialog files match `449a00a` exactly. An earlier
owner test branch lacked the ordinary-colour dependency and is preserved but
excluded from validation. The combined source is not accepted: independent
review found that an unauthorized raw String namesake can conflict with an
accepted UInt64 column before concatenation. The owner is repairing that
pre-concatenation projection; the colour allocation finding is cleared.
No combined build, PTY, matrix or release result is claimed here.

The pre-concatenation amendment `8c20342516fa44b99462afb658cbe604ac372c13`
is integrated as `7ad1741`. Unauthorized raw namesake fields/types are removed
from derived frame inputs before concatenation, while original bytes and IDs
remain intact. Accepted-vs-accepted dtype conflicts still reject. Workspace
sizing now accounts for precise integer carriers without interpreting values.
The UInt64 `9007199254740993` versus raw String regression, native dtype-conflict
test, colour memory-bound test and app test compilation passed in
`lvu-sol-union-preconcat-targeted-r2.log`; the failed r1 log is preserved.
Overlap-winner and rule-order regressions passed in
`lvu-sol-union-preconcat-prior-regressions-r3.log` using `flock --close`.
Independent final review and the actual union-colour PTY remain pending.

Final immutable review of `8c203425` composed with `26e7ea7` found no defects,
including retained attempted colour rules and publication-revision retry identity.
Report: `sol-union-review-scratch/union-colour-typed-8c203425-final-review.md`,
SHA256 `2a9a6a385410d16aeb3548c65e0b54eeffaaa04e53303e6f79529e54e73ce59b`.
The actual colour PTY then passed on production-equivalent source with test-only
`e7a8ccd`, integrated as `2d270e0`. It uses the Add mnemonic and waits for two
visible rules before checking swatches, first-match authority and restart.
Evidence: `lvu-muse-union-logs/union-colour-pty-e7a8ccd-r3.log`, exit zero;
private app SHA256 `d3ec4ea3880fc0246ea0f057191d3e898b1ae284bf967497ff68a9b565d12a99`.
The wrong-target first run and private r2 synchronization failure are preserved,
not accepted. The full combined release gate remains pending.

### v0.1.6 first full gate and diagnostic repairs (2026-09-10)

The full release gate at `18ac92ae4f82c17f80545afa47a70ca50387cefc`
failed during workspace tests; it produced no accepted record. Evidence is
`primary-astra-scratch/release-gate-18ac92ae4f82-16dfba2d2e624e51925122c0e18a0a30/commands.log`.
A subsequent diagnostic run with `--no-fail-fast` recorded 1495 passed,
14 failed and 7 ignored. Thirteen failures concern the former correlation
dialog routing in three UI test targets; their coverage repair is pending.
The remaining failure was a test calling blocking union replay inside an async
runtime. Test-only `19d3a66`, integrated as `410dda0`, uses `block_in_place`;
the focused test passed with its serialized-output budget assertions unchanged.
Production already executes replay on the dedicated union worker thread.
The diagnostic log is
`primary-astra-scratch/v016-workspace-diagnostic-596322e00a454ca6a8257eaaf5cc705d.log`.

Diagnostic Clippy found two complex type spellings and an oversized command
controller enum variant. Shared type aliases and a boxed prepared-command
payload address these without changing command state transitions or union
authority. Workspace all-target Clippy passed with warnings denied, and all six
command-controller tests passed under the canonical lock in the existing
primary diagnostic target. Evidence is
`primary-astra-scratch/v016-lint-repair-7bd71825eabf46e282f04e0e2f588033.log`.
Formatting and diff checks passed. The compiler cache limit
was reduced from 20 GiB to 15 GiB to reserve space for a fresh release target;
cache eviction preserved binaries, captures and all retained gate evidence.
Neither the diagnostic runs nor focused repairs constitute release acceptance.

Correlation coverage repair `7b2488ff5982afb7c50a316c1793b1cf0499eeac`
is integrated as `9f9f759`. It explicitly opens the legacy component for its
compatibility tests while testing new Fields actions through the shared-key
Union chooser. Frozen row/field evidence and scrolled selection are asserted.
The actual correlation PTY now normalizes differently named keys through
accepted enrichments, checks cancellation, native union membership, original
identities and restart persistence. Owner validation passed 13 correlation
component tests, 10 Fields tests, the exact scrolling test and the PTY.
Evidence: `lvu-muse-union-logs/correlation-shared-key-7b2488f-r7-rust.log`
and `correlation-shared-key-7b2488f-r6-pty.log` in the same directory.
The combined source is being frozen for a fresh full release gate; the previous
failed gate and diagnostic logs remain preserved. Independent coverage review
is proceeding separately and remains a release condition.

The full `a97ad2e0c4a08aa1b00f51988eba4808d2ffb5f0` run passed:
1510 Rust tests, zero failures, seven ignored; workspace all-target Clippy;
bridge installation/checks; both binary builds; all 80 PTY suites.
Its immutable record is
`primary-astra-scratch/release-gate-a97ad2e0c4a0-e296cce15b4f4863a2b432115197e8cb/accepted.json`;
commands-log SHA256 is
`7e07f35dec10acce2f6abc7108ce42fe9c331d169c8d08d5f37d2025a1b39859`.
This is baseline evidence only, not release authorization: independent review
identified lost automatic key pinning in newly created shared-key unions.
Legacy saved pins still restore, but no decision retired the creation behavior.

Source repair `43a8240`, integrated as `d9327d9`, initializes the new union's
pinned columns from its accepted exact key. Plain unions remain unpinned;
restore and later manual edits remain authoritative. PTY amendment `acd3ced`,
integrated as `05ab145`, checks creation, cancellation preserving origin pins,
manual extra pins and a key unpin surviving restart. Focused runtime validation
and independent review remain pending; the amended source needs a full gate.
The compiler cache cap is now 10 GiB to reserve that next target without
deleting retained binaries or proof. Its lock-coordinated restart is logged in
`primary-astra-scratch/sccache-10g-08fcfc4e9fd34a768f1d6ad3a739431d.log`.
