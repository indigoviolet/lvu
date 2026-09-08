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
