# lvu checklist

Current app: **preview 051**. This is the single feedback and work list.

**Done** = available now. **Ready** = tested, awaiting release.
**Working** = being implemented or validated. **Open** = unfinished.
**Decision** = proposal awaiting your decision. Evidence belongs in the [work ledger](docs/work-ledger.md), not this list.

| Status | Request |
| --- | --- |
| **Done** | Sixteen-colour terminals: identities map by hue onto the six usable ANSI colours with bold as a second axis, levels and JSON kinds take their ANSI names, the contrast floor is measured against xterm's palette, and Details matches through the shared styling functions. Chrome (selection, default-button fill, borders) at sixteen colours is the next pass (W23). |
| **Done** | Misdiagnosis corrected: a restarted source resumes from its durable cursor and repeats nothing (8,192 of 8,192, 8/8 runs with fragments off). The 0–2 extra records are partial-line fragments captured before their terminator, which an uninterrupted run produces just as often. Stated in AGENTS.md; pinned from both sides in `restart_replay_bound.rs`. |
| **Working** | First public release `v0.1.0`: W26 validates archives, formula and mise install, dry-runs the release workflow, then tags main, publishes the release, and pushes the formula to the existing `indigoviolet/homebrew-tap`. macOS archives ship tentatively, marked untested, for the user to try. (priority) |
| **Working** | Resume the most recent set of sources by default: `lvu` with no arguments re-acquires last session's sources (files re-tailed, commands restarted), `--fresh` starts empty, arguments add to the resumed set; stdin cannot resume and says so. (W25) |
| **Done** | The Details pane colours a record exactly as the log pane does: the token→colour rule exists once (`json_kind_style`), the raw line and flat values go through the log's text styler, and rows inherit level and colour-field styling through `record_style`. |
| **Open** | After a Cargo feature change (Polars `extract_jsonpath`) the primary target served stale `lvu`/`lvu-app` binaries under sccache: two PTY suites failed deterministically until `cargo clean -p lvu -p lvu-app`. Every agent's gate builds the same way; find whether sccache or incremental fingerprints are at fault and make the matrix preflight detect a binary older than its sources. |
| **Done** | Editors no longer type the bare character of an unbound Alt-chord (all nine text-field components share one predicate); the loop probe reports dispatch time; two PTY stories that sent Esc and a key back to back (read as Alt-key) now wait for the layer to close. |
| **Working** | Restarting a source under load fails about one run in three with `Journal(AlreadyOpen)`: the lock holder is the same process, so some in-process handle outlives a stop that reported complete. Reproducer in `crates/lvu-ingest/tests/restart_race.rs`; W13 root-causing with wall-clock sampling. No retry: a retry would turn a leaked handle into a stall. |
| **Done** | The search-race timeout was the story, not the app: it sent Esc and `t` back to back, which a terminal reads as Alt-t, so the Esc never closed the editor. The Time dialog opens in 10–66 ms and reads nothing blocking. 0 failures in 40 runs under load, against 6 before. Same hazard being fixed in the enrichment command story, plus editors ignoring unbound Alt-chords. (W15) |
| **Done** | The janitor reclaims a worktree's cargo target once the worktree is gone, or its branch is merged and the target untouched for six hours; `disk:check` covers the build volume. sccache capped at 10 G. |
| **Working** | Saving a new enrichment step on a large source gives no feedback: the step is added but the editor stays open silently, and a second Enter reports a duplicate field. Save must show Evaluating, block a second Save, close to the list on acceptance and stay with the error on rejection. (W19) |
| **Working** | Folding by a single column: the fold key is any column, defaulting to a derived pattern column (normalised text + level, today's behaviour); choosing fields is just an enrichment column built from them, and no normalisation applies to fields not included. A Folding dialog sets key column, minimum run, scope and normalisation per view. (W23) |
| **Done** | Folding no longer blanks the pane: on a 4.8M-row capture every frame was blank for 180 s, now none. The fold feed starts at the visible window instead of position 0, stops evicting the viewport's row cache, and the last usable viewport is kept with `fold: N runs, M hidden, folding K more` in the status. Gutter indicators for folded extent are next (W22). |
| **Done** | Every dialog has one default action, filled in the accent colour, run by Enter from anywhere except inside a prose field (Ctrl-Enter there), an expression field (applies), or a control that consumes Enter. Enrichment: Add when empty, Edit with a step selected. Lists open on the relevant or first row. Audit in docs/dialog-default-actions.md. Found View's Apply button vanishing at 80x24. |
| **Done** | Discoverability rule §8.10: Alt mnemonics underlined on their buttons, the palette is the exhaustive index with the chord for the current focus, Help indexes base-screen keys once under Conventions, routine key reminders and all footers removed. Found three palette chords that were never bound and four base operations with no palette row. Audit in docs/dialog-discoverability.md. |
| **Done** | The Add source dialog no longer jumps while typing a path: it moved 4 rows and resized by 8 twice per keystroke. Rule §5.2.1: a live region (completion list, candidates, preview) gets a fixed row count when the dialog opens; overflow scrolls, underflow stays blank with its heading. Step editor fixed under the same rule. |
| **Done** | `lvu-discovery` docker tests flaked under fork pressure: a sibling test's fork inherited a fixture still open for writing (`Text file busy`). Every fixture-writing or forking test now holds the lifecycle lock; 0/80 failures where before 2/78. The product had reported the OS error correctly. |
| **Done** | Command sources no longer outlive the app: killed with it on quit, SIGTERM and SIGKILL (parent-death signal plus process-group kill), the PTY harness reaps the app's session on every exit path, and the janitor sweeps orphans older than ten minutes. Found nine more orphans from the source-control suite on the way. |
| **Done** | PTY matrix scheduled for a shared machine: longest suites first, spaced starts, the three contention-sensitive suites serialised; 51/51 at load 14 where the old runner got 49/51 at load 9, in 100 s. Summary prints per-suite wall time and load. |
| **Done** | The "7 s to quit under load" was a test race, not the app: the slowest input-loop iteration is 40 ms at load 12 and quit takes 10 ms; the story's `q` landed in a surface Escape had not yet closed. Storage, settings and command settles now shut down concurrently (8 s worst case → 3 s). |
| **Open** | Restarting a source right after `stop()` reports `complete` fails with `Journal(AlreadyOpen)` under load: the journal lock is still held. Reproduced twice — `live_view.rs:3310` in a full-workspace run, and `crates/lvu-ingest/tests/restart_race.rs` at attempt 62 of 200. A probe naming the lock holder shows it is *this* process (`self=494583`, `pid 494583 fd 11`), so it is neither cross-process contention nor descriptor inheritance; some in-process handle outlives the stop that reported complete. Two hundred cycles at rest pass, a forking neighbour does not reproduce it, and lifecycle probes perturb the timing enough to hide it. Next: sample `run_writer` entry/return and the stop reply against a wall clock rather than printing, so the ordering survives instrumentation. |
| **Open** | The three AI shutdown settles (investigation, source-ai, ai) are still serial and add 9 s to the worst-case shutdown bound; they share `self.agent` and need their own assignment with the AI suites as the gate. |
| **Done** | An unaccountable derived-index cache total no longer refuses new sources: rows are served and the view status says `index cache total unverified`. PTY runs no longer leak indexes into `~/.cache/lvu`; the janitor reclaims orphaned ones. |
| **Done** | Forked views appear directly after their origin, and `[`/`]` cycling or a sidebar click counts as the last-used view on restart. |
| **Done** | Escape on a search editor no longer discards a filter you already applied on All events. Opening a saved investigation returns to its transcript. PTY matrix 50/50 twice on the same tree. |
| **Done** | Restructure the enrichment dialog into two layers: a step list with Add/Edit/Remove, and a focused step editor showing input, expression and output, saving back to the list. |
| **Done** | Ask 🧠 dialog rebuilt on the anatomy: multiline Request scrolls vertically; a prepared task states what it will do with no kind selector. |
| **Done** | Bridge failures now say what actually failed: resources missing, unbuilt, launcher absent, daemon unreachable, or provider unauthenticated. |
| **Done** | A transient derived-index lock no longer kills the source worker; it retries with jittered backoff and the pane reports the wait. |
| **Done** | lvu emitted a bare `\x1b[2J` outside the synchronized-output block on resize, so a real terminal flashed the whole screen. The resize clear now happens inside the same synchronized update as the frame that repaints it. |
| **Done** | Dialogs size to their content; no dead rows at 54x16 across every adopted dialog. |
| **Done** | Discovery's bounded procfs scan gives up under process pressure, returns 0 candidates and reports `file descriptor limit reached` — which is its own budget, not the system limit (1M, 4.3k in use). Misleading on a busy machine. |
| **Done** | `test_discovery_contrast_pty` depends on discovery finding candidates, so it fails when the machine is loaded. Make it tolerate a limited scan. |
| **Working** | Convert dialogs to owned components (docs/component-model.md). Done: 14 of 16 dialogs plus Folding and the fork subsystem inside `Views` (`Action` 194→~80, `Focus` 22→8, `app.rs` 13.9k→8.8k, `ui.rs` 8.2k→4.0k lines). In flight: Ask/Investigation (W14). Raw context held pending the `o` decision. |
| **Done** | Every dialog (18 of 18) is on the shared anatomy and size classes: Investigation split into Question, provenance and a `New │ Saved` pane; palette columns fixed and right-aligned; External command on labelled fields with a results pane. |
| **Done** | Fix the Source 🧠 proposal being unreviewable at small terminal sizes: launch/cwd/restart/env/why must be reachable before Start reviewed. |
| **Done** | Fix silent data loss: with a legacy `.lvu-captures` dir in the cwd, the capture root flips between runs and abandons sources, workspace, filters and enrichment. |
| **Done** | Restore a visible Apply action in the grouping dialog: the universal-shortcut cleanup removed the `Enter Apply` hint without replacing it, so the dialog has no actions region. |
| **Done** | Reconcile the PTY matrix against the new dialog vocabulary; 37 of 42 suites pass. |
| **Done** | Fix the five long-standing PTY failures, including a JSON fixture that never had a build target. |
| **Done** | Enrichment persists an unfinished *edit* draft across restart, alongside an unfinished new step. |
| **Done** | Give the grouping `[ Apply ]` action a mouse hitbox. |
| **Done** | Adopt the dialog anatomy in every dialog: Search, Advanced, Grouping, Time, Enrichment, External command, Add source, View, Recipes, Bookmarks, Fields, Raw context, Storage, Settings, Help, palette, Ask, Investigation. |
| **Done** | Offer `[ Go to ]` in Bookmarks. Honest since bookmarks became source-scoped: it selects the record in its source's All events view, where it is always present. |
| **Done** | Every saved recipe revision carries its date (additive, no schema bump, undated revisions serialise byte-identically); the Recipes list shows it in §12.9's date column in the display zone. |
||||||| parent of 930f876 (Reconcile the PTY matrix with the dialog system, and make Apply clickable)
| **Working** | Redo the dialog design system — hierarchy, spacing, sizing and styling — so dialogs read like a modern GUI rather than ad-hoc panels. |
| **Working** | Make `q` and Esc close **one layer at a time**: selection, dropdown, dialog, Details, then app. Keep `q` as text while editing. |
| **Working** | Use the **same buttons, inputs, dropdowns and layout rules in every dialog**, including Source and Time. |
| **Working** | Give shortcuts one consistent color everywhere; do the same for descriptions/help, labels and status. |
| **Working** | Keep help, diagnostics and disabled-action explanations readable in both dark and light themes. |
| **Working** | Align command-palette names and shortcuts; put descriptions in a separate readable area. |
| **Working** | Remove the wall of disabled commands from the default palette; explain unavailable search matches clearly. |
| **Working** | Finish the Ask form: visible actions, kind dropdown, multiline request, separate status and readable submitted request/activity. |
| **Working** | Apply shared controls to Time and Settings without losing their working form behavior. |
| **Working** | Apply shared controls to Source, enrichment and external-command forms. |
| **Working** | Apply shared controls to Recipes, Views, Bookmarks and notes. |
| **Working** | Apply shared styles/layout to Fields, Details, Context, Storage, Help and all diagnostic/fallback surfaces. |
| **Done** | Investigate blank restored filtered views, including cases where the query reports matches but no rows appear. |
| **Working** | Match file suggestions using space-separated AND terms, glob patterns, and bounded cancellable `**` recursion with clear partial results. |
| **Working** | Fix Source typing losing literal `q` while automatic suggestions are scanning or visible; preserve Escape completion-first dismissal. |
| **Done** | Complete file paths automatically while typing; remove Complete path and Open buttons; Enter opens the selected file or enters a directory. |
| **Done** | Restore Up/Down selection in discovery and file suggestions directly from the search/input field; Enter opens the selected result. |
| **Done** | Let a *text* enriched column be declared as the event-time basis. `TimeFieldSelection` now carries the chrono format as a fifth token part (four-part tokens still parse), so `ColumnTimeInterpretation::Text` round-trips. The format is inferred from the bounded sample the dialog already reads — RFC 3339, ISO with and without a zone, Apache/CLF — measured by the parser that will actually run, and shown in the confirmation step with its sample and match rate as an editable field: `FormatRequired` is a suggestion, not a refusal. Values the format cannot read become nulls counted through `event_time_invalid`, never dropped. Syslog is recognised and explained instead of offered: it carries no year and a Polars format cannot supply one. |
| **Done** | Every source has a permanent All events view; applying a filter creates one editable view; restart reopens your last-used view. |
| **Done** | Bookmarks are source-scoped and jump to the record in All events; raw context remains available. |
| **Open** | Let short AI requests inspect more data when the bounded prompt sample is insufficient. |
| **Done** | Explain raw-row loading/index failures instead of showing only “query ready.” Wired into the status line; the API alone had shipped with zero callers. |
| **Done** | Load-dependent PTY failures explained: pyte lacked DEC 2026 support (harness), and a migrated workspace DB left by a reverted commit poisoned every suite without its own capture root. Matrix is 46/46. |
| **Done** | Intermittent empty reopen was the derived-index lock defect; fixed with a deterministic regression. |
| **Done** | The Time-test shutdown failure was the same poisoned-workspace nonzero exit; fixed. |
| **Done** | Fixed the hang where a resize immediately followed by a keypress wedged the app — process alive, terminal left in the alternate screen. crossterm loops `read(2)` until its parser yields an event and stops only on `WouldBlock`, which the blocking standard input never reports, so a read carrying only the start of an escape sequence slept inside `event::poll` with the poll timeout unexpired and no frame or restoration possible. lvu now hands crossterm a private non-blocking view of the terminal. Not dialog-specific; this was the earlier Settings resize/Enter observation. |
| **Working** | Let multiple lvu windows automatically share a background capture worker; independent views, detach on close, stop after the last window. |
| **Done** | Support installation through Homebrew. |
| **Done** | Support installation through mise, including relocatable helper/runtime resources. |
| **Open** | Validate installation/terminal/process behavior on macOS and Windows. Audit done (docs/portability.md); no macOS/Windows run yet. |
| **Done** | Field correlation across sources: `r` in Fields resolves the record's typed value, maps each source's own field name explicitly, and opens a merged view of every matching record with the correlating fields pinned. Bounded cancellable lookup, persisted across restart. |
| **Open** | Extend command enrichment to multiple/interleaved steps, downstream queries and recipes. |
| **Done** | Dataset-relative time ranges (first-to-last, last N of data, ± around the selected record) labelled by which clock they mean; `{`/`}` jump to the record after the next quiet gap, threshold per view in the Time dialog. |
| **Working** | Explicit time display and sort: order shown as capture (arrival) in the Time dialog and status line; display zone in Settings, default UTC, shown read-only beside the basis; fixed offsets only, no DST. (W19) |
| **Working** | A merged multi-source view concatenates by source instead of interleaving by time. Designed in `docs/merged-view-ordering.md`: a k-way merge over the per-record basis times `SourceMatches.times` already carries, materialised once per publication, with eight invariants stated (per-source order preserved, untimed records keep their place, `index_of_id` stays O(k + log n), fold runs contiguous in merged order, identities stable). `crates/lvu-view/tests/merged_ordering.rs` has three live tests pinning what must not break and five `#[ignore]`d ones that fail today and are un-ignored by the implementation. Implementation waits for W24: the order is built in the publication path `perf/query-latency` is changing. |
| **Done** | Support selecting arbitrary timestamp fields, beyond the accepted `timestamp_utc` enrichment. |
| **Done** | Improve automatic timestamp/epoch recognition without requiring AI. |
| **Done** | Fall back gracefully on 256-colour terminals: without a `COLORTERM` truecolor claim, identity colours are chosen from the xterm colour cube and checked for contrast there, so what lvu measured is what the terminal displays. The 16-colour case is not covered — the cube is assumed available. |
| **Open** | Explain the flaky `snapshot_packs_many_evaluation_batches` lvu-view test; it fails ~1 run in 3 under parallel load on baseline. |
| **Working** | Add predicate color rules and regex span highlighting. (W18) |
| **Done** | Nested JSON is a tree in Details and Fields (collapse to `{3 keys}`/`[12]`, expand in place, bytes untouched). Fields has a Value pane: inferred type with share and sample, present/distinct counts, top five values, min to max, and one-key Pin/Filter/Exclude/Color/Fold/Correlate. The editors' completion offers nested paths so a path is never typed. Nested filtering is lexical until Polars JSON-path lands (W21, in flight). |
| **Done** | Repeated-pattern folding, off by default, wired into the viewer with editor completion, recipe suggestion and step preview reading the unfolded page. |
| **Done** | Add HTTP/reconnect sources and explicit command restart policies. |
| **Done** | Add ownership-aware capture/investigation deletion, retention and cache-pressure handling. |
| **Ready** | Soak and slow-terminal harnesses: `mise run soak`, `soak:long`, `soak:slow-terminal`. Multi-cycle runs complete: six cycles over 620k records and a restart, 36 timed filters, clean shutdowns. Memory answers the leak question — after warm-up the steady state is 103.7, 95.0, 92.1, 92.1, 92.2 MiB, journal 109.0 to 109.9 MiB (the chatty command source appending), derived index 24.0 to 24.3 MiB; nothing climbs. Shutdown 0.445s worst, slowest input-loop iteration 0.082s. |
| **Working** | (W22) A 512 MB source shows an empty log viewport for at least fifteen minutes while capture runs, and the status line advertises the rows it is not drawing: `FOLLOW | raw view | 473050-473088/473088` over a blank pane, with the source pane reporting 1,368,935 records captured. This is "display raw data before derived data is ready" failing at scale, and it is the same shape as the blank-pane defects fixed earlier, now during first capture rather than after a restart. Reproduce: `mise run soak -- --mode long --bytes 536870912`. |
| **Working** | (W22) `RowReadiness::RowsPending` and `Indexing` are deliberately kept out of the status line as transient. At 512 MB they are not transient: the pane above is blank for minutes with nothing said. Revisit that exclusion once the blank viewport itself is understood — an explanation is not a substitute for the rows, but silence is worse than either. |
| **Working** | (W24) Filtering waits about 60,000 records per second and uses almost no CPU. Measured after capture settled, on an otherwise idle 16-core box: 152k records take 2.42s and 620k take 10.75s (p50), the same within 2% whether the filter matches 7 rows or most of them, at 0.18s of CPU across 10.57s of wall clock. So it is not scan throughput and not the machine — something blocks per record or per batch. A 3 GB source would be roughly eight minutes per filter. Evidence: `mise run soak`, and `tests/soak/soak.py` records p50/p99 per run. |
| **Open** | Capture reads about 10 MB/s and writes a journal half again the size of its input (109 MB for 64 MB, 244 MB for the first 1.37M records of a 512 MB source). Worth separating read cost from journal write cost against a plain `cat` of the same file. Evidence: every `mise run soak` sample line. |
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
