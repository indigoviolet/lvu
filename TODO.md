# lvu checklist

Current app: **preview 056**. This is the single feedback and work list.

**Done** = available now. **Ready** = tested, awaiting release.
**Working** = being implemented or validated. **Open** = unfinished.
**Decision** = proposal awaiting your decision. Evidence belongs in the [work ledger](docs/work-ledger.md), not this list.

## Current app: preview 056

What a user of the current build needs to know: what is being worked on, what is unfinished, and what waits on a decision, by area. Wording is unchanged from when each row was written.

### Viewing

| Status | Request |
| --- | --- |
| **Working** | One place to see every operation applied to a view: a read-only summary in evaluation order (sources, role, filter, advanced, grouping, time window and basis, enrichment chain, fold, colour rules, pinned columns, readiness), Enter on a row opens the owning dialog. (W28, Fable) |
| **Working** | Add predicate color rules and regex span highlighting. (W18) |

### Time

| Status | Request |
| --- | --- |
| **Open** | The `Extracted` time basis parses the `timestamp_utc` derived column with `lvu::parse_utc_nanos` rather than a Polars expression, though the value is already a column — the same job `ColumnTimeInterpretation` compiles for a chosen column. Found by the principle check in `docs/merged-view-ordering.md`; pre-existing and independent of the ordering work. |
| **Open** | A recipe whose filter names a column its own enrichment chain has not produced shows Polars' lowering error verbatim — `expression cannot be lowered for this schema: unable to find column "error_flag"` — in the Enrichment dialog. Seen by W13 in `test_lvu_real_pty`'s recipe adaptation under load, so the chain and the filter it was compiled against can disagree at least transiently; the engine's `dependency_unavailable` guard only covers a stage that *ran and failed*, not one absent from the chain. Two halves: find why the pair can disagree, and make the message name the cause (`error_flag is not produced by this chain`) rather than the implementation. Pinned by `a_filter_naming_a_column_no_stage_produces_reports_polars_lowering_verbatim` in `crates/lvu-query/tests/query.rs`. |
| **Open** | Display time zones are fixed UTC offsets only: no timezone database, so daylight saving is never applied and a named zone (`Europe/Berlin`) cannot be chosen. Every displayed timestamp carries its offset so nothing is silently wrong, and the Settings help line says so. A real tzdb would also let the Time dialog accept named zones on input, where the same limitation already applies. |

### Dialogs

| Status | Request |
| --- | --- |
| **Open** | The base screen's `Alt-S` / `Alt-R` (stop / restart the selected source) have the problem the dialog mnemonics had: an xterm's default sends them as the letters `ó`/`ò`, never as a chord, so on that terminal they are reachable only through the palette. They are base-screen keys, not action-row mnemonics, so W27's rule does not reach them; they need a non-Alt spelling. |
| **Working** | Convert dialogs to owned components (docs/component-model.md). Done: every dialog except Correlation (Raw context was deleted in favour of the `o` jump); `Action` 194 → under 60, `app.rs` 13.9k → ~8k lines. Correlation in flight (W21); then the migration bridge goes and §6 records the final numbers. |

### Sources and capture

| Status | Request |
| --- | --- |
| **Open** | Let multiple lvu windows automatically share a background capture worker; independent views, detach on close, stop after the last window. |

### Assistance

| Status | Request |
| --- | --- |
| **Working** | The three AI shutdown settles (investigation, source-ai, ai) are still serial and add 9 s to the worst-case shutdown bound; they share `self.agent` and need their own assignment with the AI suites as the gate.  (W14, built, in gate) |
| **Working** | Let short AI requests inspect more data when the bounded prompt sample is insufficient: a wider sample tier offered when coverage reports omissions or the answer asks for more, size and cap shown, transcript records which sample each answer used. (W14, built, in gate) |

### Performance

| Status | Request |
| --- | --- |
| **Working** | Whole-view field statistics: engine aggregation and the bounded, cancellable, superseded worker are in; app wiring, the Value pane heading (`all N records`, exact distinct counts replacing the 4,096 floor, readiness while pending) and the Fields PTY story remain. (W24) |
| **Working** | Capture's remaining cost is the record-at-a-time handoff, measured at about 2.1 us of user CPU per record. The framer already produces a whole read chunk's records as one vector, then sends them through the acquisition channel one at a time, each with its own semaphore permit and wakeup, and the writer drains at most 64 before appending. Widening what the pipeline moves at once is worth 22.4 to 30.6 MB/s with durability out of the picture and cuts system CPU by 60%; the read size alone changes nothing (256 KiB reads measured 18.2 MB/s against 18.5 for the shipped 16 KiB), so the cost is per record. Doing it needs `writer_queue_capacity` bounded by bytes rather than records first, or the memory bound weakens: 128 records at the 64 KiB record ceiling is 8 MB today and 4,096 would be 256 MB. A bare read of the same file runs at 337-421 MB/s, so nothing here is disk-bound. Evidence: `cargo test -p lvu-ingest --test throughput -- --ignored --nocapture`.  (W25) |
| **Open** | The writer thread serves every page read between appends, so a query paging the journal waits behind capture's own appends. Not addressed by the commit work; measure before assuming it matters. |
| **Working** | (W22) A large source in FOLLOW shows an empty viewport while capture runs: the provider's stale-window backlog is fixed (`1fc1dd0`, queued), but the pane stays blank because FOLLOW sets `top` from a total that indexing keeps advancing, so the requested window's rows never arrive in time. Fix: FOLLOW anchors to the newest servable window and its tail row is the selection for record-scoped operations; W24's two-minute probe is the reproducer. |
| **Working** | The merged-view display order is rebuilt whole on every publication, which is the cost W24's `Appended` refresh work removes for `sequences`/`times`. Against W24's `crates/lvu-view/tests/refresh_cost.rs` (not yet on main) a refresh over 160,000 rows measured 3.05x one over 20,000 for 8x the view. Fix: extend the order instead of rebuilding it. When every source's appended tail begins at or after the largest existing merge key — checkable in O(k), and the common live-tail case — the previous order is a prefix of the new one and only the tail needs merging; a late older record falls back to a rebuild from its insertion point. W24 lands first, then this.  (W15, in progress; W24's `refresh_cost` test is ignored until it lands) |
| **Open** | Express case-insensitive search as a `(?i)` regex instead of lowercasing the column first: 414 MB/s to 1,516 MB/s on the dominant string operation, measured in `crates/lvu-query/tests/binary_column_scan.rs`. One expression change, no new concepts, and it moves the scan's floor before anything larger is considered. Found while writing `docs/columnar-cache.md`. |
| **Working** | (W22) `RowReadiness::RowsPending` and `Indexing` are deliberately kept out of the status line as transient. At 512 MB they are not transient: the pane above is blank for minutes with nothing said. Revisit that exclusion once the blank viewport itself is understood — an explanation is not a substitute for the rows, but silence is worse than either. |

### Tooling and release

| Status | Request |
| --- | --- |
| **Working** | Cut `v0.1.1` from main (`38bb2d2`): version bump, tag, workflow publishes the four archives including Linux arm64, formula to the tap, release notes listing what changed since 0.1.0, clean-user verification. (W26) |
| **Ready** | Linux arm64 archive (`aarch64-unknown-linux-musl`, native on GitHub's arm64 runner) is in the release matrix, formula and docs; the dry run built and executed all four archives (arm64 compiled a real Polars expression and loaded the bridge). Ships with `v0.1.1`. Windows is not feasible without product work: two crates import Unix-only modules unconditionally. |
| **Open** | `test_empty_event_fields_pty.py` loses its 3 s wait for `Fields closed` at load 15 or so, independent of any recent change: interleaved runs of the same suite against the pre-fix and post-fix binaries under one load failed 1 in 8 each. It needs the treatment the other waits got — a bound tied to something the app actually signals, not a wider number. |
| **Open** | Validate installation/terminal/process behavior on macOS and Windows. Audit done (docs/portability.md); a macOS checklist an agent can run is docs/mac-test-plan.md; no macOS/Windows run yet. |

## Shipped since preview 047

Done rows by the preview that first carried them (the publication commit after the row was marked Done). Older Done rows are in the [work ledger](docs/work-ledger.md) under `2026-09-08 — checklist rows shipped before preview 047`.

### previews 053–056 (2026-09-08)

Rows marked Done since preview 052, grouped by the area they came from.

| Status | Request |
| --- | --- |
| **Done** | Raw context is a jump, not a dialog: `o` on a filtered view jumps to the record in its source's All events view (centred, `raw of <view> · #n · o back` in the status line), `o` again returns; Fields and Bookmarks reopen on return; merged views jump to the record's own source. The dialog, its focus, actions and class XL are deleted. |
| **Done** | The Fields dialog's Fold button follows state: it reads Unfold when the view is already folded by that column and switches the key when folded by another, naming the change. |
| **Done** | A merged multi-source view interleaves by time on event, extracted and chosen-column bases; on the capture basis it keeps the user's source order. The order row says `capture · source order`, `recognized · merged`, or names how many sources arrive out of order. Eight invariants tested; a grouped view merges groups, not records; the real app's provider wrapper had been dropping `view_order` silently, caught by the PTY story. |
| **Done** | Search and Advanced are two tabs of one Filter dialog (`docs/dialog-system.md` §12.1): `/` opens on Search, Alt-A / click / Tab+arrows / the palette row `Filter › Advanced` reach Advanced; the title names every applied constraint from the same `ViewState` fields the view summary reads; `[ Apply ]` filled, `[ Clear ]` per tab; both constraints still apply at once, AND-ed; `p` retired. (W29, Fable) |
| **Done** | Saving a new enrichment step on a large source gives no feedback: the step is added but the editor stays open silently, and a second Enter reports a duplicate field. Save must show Evaluating, block a second Save, close to the list on acceptance and stay with the error on rejection. (W19) |
| **Done** | Folding by a single column: the fold key is any column, defaulting to the derived pattern column; a Folding dialog sets key, minimum run, scope and normalisation per view; `[ New column… ]` builds an enrichment column from chosen fields. Shipped in preview 052. |
| **Done** | Explicit time display and sort: order shown as capture (arrival) in the Time dialog and status line; display zone in Settings, default UTC, shown read-only beside the basis; fixed offsets only, no DST. |
| **Done** | The startup heart under a dialog scrim was half-erased: the scrim repainted only the foreground of half-block cells. It now dims both pixels a third of the way toward the backdrop, shape preserved; a block-drawing cell is a picture, not text (§6.2). |
| **Done** | Bare underlined letters press dialog buttons when no text field has focus; Alt stays as the text-field path and `lvu --keys` shows what a terminal sends. Alt was never deliverable on xterm defaults (Alt-f arrives as `æ`). Shipped in preview 055. |
| **Done** | Help lists no command-line options; `lvu --help` and the README own the CLI. Shipped in preview 055. |
| **Done** | Redo the dialog design system — hierarchy, spacing, sizing and styling — so dialogs read like a modern GUI rather than ad-hoc panels. Shipped by the dialog anatomy (047), layered dismissal (042), default actions and discoverability (051), the Ask rebuild (048) and the palette rework (048). |
| **Done** | Make `q` and Esc close **one layer at a time**: selection, dropdown, dialog, Details, then app. Keep `q` as text while editing. Shipped by the dialog anatomy (047), layered dismissal (042), default actions and discoverability (051), the Ask rebuild (048) and the palette rework (048). |
| **Done** | Use the **same buttons, inputs, dropdowns and layout rules in every dialog**, including Source and Time. Shipped by the dialog anatomy (047), layered dismissal (042), default actions and discoverability (051), the Ask rebuild (048) and the palette rework (048). |
| **Done** | Give shortcuts one consistent color everywhere; do the same for descriptions/help, labels and status. Shipped by the dialog anatomy (047), layered dismissal (042), default actions and discoverability (051), the Ask rebuild (048) and the palette rework (048). |
| **Done** | Keep help, diagnostics and disabled-action explanations readable in both dark and light themes. Shipped by the dialog anatomy (047), layered dismissal (042), default actions and discoverability (051), the Ask rebuild (048) and the palette rework (048). |
| **Done** | Align command-palette names and shortcuts; put descriptions in a separate readable area. Shipped by the dialog anatomy (047), layered dismissal (042), default actions and discoverability (051), the Ask rebuild (048) and the palette rework (048). |
| **Done** | The palette's default list shows only operations that can run now; a search that matches an unavailable one lists it under `Not available now` with the same reason the disabling control declares, and Enter there is inert. |
| **Done** | Finish the Ask form: visible actions, kind dropdown, multiline request, separate status and readable submitted request/activity. Shipped by the dialog anatomy (047), layered dismissal (042), default actions and discoverability (051), the Ask rebuild (048) and the palette rework (048). |
| **Done** | Apply shared controls to Time and Settings without losing their working form behavior. Shipped by the dialog anatomy (047), layered dismissal (042), default actions and discoverability (051), the Ask rebuild (048) and the palette rework (048). |
| **Done** | Apply shared controls to Source, enrichment and external-command forms. Shipped by the dialog anatomy (047), layered dismissal (042), default actions and discoverability (051), the Ask rebuild (048) and the palette rework (048). |
| **Done** | Apply shared controls to Recipes, Views, Bookmarks and notes. Shipped by the dialog anatomy (047), layered dismissal (042), default actions and discoverability (051), the Ask rebuild (048) and the palette rework (048). |
| **Done** | Apply shared styles/layout to Fields, Details, Context, Storage, Help and all diagnostic/fallback surfaces. Shipped by the dialog anatomy (047), layered dismissal (042), default actions and discoverability (051), the Ask rebuild (048) and the palette rework (048). |
| **Done** | Quitting lvu while a short-lived command source was finishing reported `source is not active` and exited non-zero: shutdown snapshotted live sources, then treated a source that had ended on its own as a failure to stop. Shutdown now waits for the source's own terminal state and reports its outcome; 30/30 under load where 7/10 failed before. Found through W25's resume test. |
| **Done** | `lvu` with no arguments resumes last session's sources through the sidebar's Restart path: files from the durable cursor, commands with their recorded cwd and environment, a status notice with the counts; `--fresh` acquires nothing and deletes nothing; arguments add to the resumed set; `--resume` is the explicit default. Session record is a plain file beside the workspace database, no migration. |
| **Done** | Fixed: a forked child inherited the journal's flock through the pre-exec window, so a stop-and-restart inside it was refused (11 of 20 runs under fork pressure, 0 without). Ownership now uses an in-process claim registry plus a POSIX record lock that fork does not pass on; a storage probe that would have destroyed the lock it checked was fixed too; the reproducer is in the gate. Earlier: Restarting a source under load fails about one run in three with `Journal(AlreadyOpen)`: the lock holder is the same process, so some in-process handle outlives a stop that reported complete. Reproducer in `crates/lvu-ingest/tests/restart_race.rs`; W13 root-causing with wall-clock sampling. No retry: a retry would turn a leaked handle into a stall. — Earlier entry: Restarting a source right after `stop()` reports `complete` fails with `Journal(AlreadyOpen)` under load: the journal lock is still held. Reproduced twice — `live_view.rs:3310` in a full-workspace run, and `crates/lvu-ingest/tests/restart_race.rs` at attempt 62 of 200. A probe naming the lock holder shows it is *this* process (`self=494583`, `pid 494583 fd 11`), so it is neither cross-process contention nor descriptor inheritance; some in-process handle outlives the stop that reported complete. Two hundred cycles at rest pass, a forking neighbour does not reproduce it, and lifecycle probes perturb the timing enough to hide it. Next: sample `run_writer` entry/return and the stop reply against a wall clock rather than printing, so the ordering survives instrumentation. |
| **Done** | Match file suggestions using space-separated AND terms, glob patterns, and bounded cancellable `**` recursion with clear partial results.  Shipped in the source-direct completion work (preview 043); recorded in the ledger archive. |
| **Done** | Fix Source typing losing literal `q` while automatic suggestions are scanning or visible; preserve Escape completion-first dismissal.  Pinned by `tests/pty/test_source_direct_typing_pty.py`. |
| **Done** | Incremental refresh: membership publishes as merging immutable chunks, so a capture refresh adds a chunk instead of rebuilding; refresh CPU 0.207 → 0.071 per second at 620k and flat across view sizes on W24's tree. On main the merged-view order is still rebuilt whole per publication (3.4x for 8x the view), so `refresh_cost.rs` is ignored until W15's O(k) prefix extension lands and un-ignores it. Soak surfaces identified by framed title; `soak:long` sized against the capture rate. Columnar-cache design note in docs/columnar-cache.md. |
| **Done** | Capture committed every few scheduler batches (23 fsyncs per MB, plus one per checkpoint), so under contention it waited ~460 ms per MB on fsync. Group commit now: one commit per 4,096 records or 500 ms, the file cursor written only behind the commit that covers it; 2.5 commits per MB, capture settled 66 → 39 s on the soak, shutdown 0.9 → 0.1 s; a guard asserts commits per MB. Next: framing and encoding cost 36 ms/MB against 2.4 ms to read. |
| **Done** | (W25) Capture's cost was durability, and the fsync rate was not a property of the data. The writer committed every `sync_every_batches` (8) *batches*, and a batch is however many messages happened to be queued when it woke, so the commit rate tracked the scheduler: one commit per 442 records, 23.2 per MB, plus an unconditional fsync per 256 KiB file checkpoint. Group commit — one per 4,096 records or 500 ms, whichever first — makes it 2.5 per MB, one per 4,107 records, a 9.3x reduction. The file cursor now rides on that decision instead of forcing its own: a checkpoint is held until the commit that covers it succeeds, so the cursor is never ahead of durable data. Two per-record costs went with it: a `stat` after every appended record for a length the journal already knew, and one `write` per record after allocating a body and a frame vector for it. The commit-count reduction is what removes the exposure to contention, where `fsync` measured a 19.8 ms median: 23.2 commits/MB is 460 ms of waiting per MB, 2.5 is 50 ms. The journal being half again the source stays arithmetic, not a defect, and the throughput test holds that constant to 80 bytes per record so it stays one. Phase table and both load regimes: `docs/performance.md`. Regression coverage asserts bytes per CPU-second and commits per MB, never wall clock: `crates/lvu-ingest/tests/throughput.rs`. |
| **Done** | The soak's `Fields · record` wait was the driver keying on content instead of the framed title; fixed (W24). The underlying FOLLOW anchor is W22's row above. |
| **Done** | (W24) Historical filter latency. The earlier reading — that filtering blocks per batch and uses 2% of a core — did not survive measurement: sampling the app's own `/proc/<pid>/stat` across a filter showed 15.5–21.6s of CPU across 11.1–11.8s of wall clock. It was compute-bound in unoptimised dependency code, over 256-record pages, allocating a UUID string per scanned row twice and hashing every scanned record. The same 620k capture now filters in 0.71–0.81s at 705k–838k records per CPU-second, a 15x wall and 21x CPU-normalised improvement. `mise run soak` p50 10.75s to 0.727s and p99 11.05s to 0.824s on a quiet box, at 1.10 CPU-seconds per query-second, which clears the soak's own one-second query budget. Phase table, causes and what bounds it now: `docs/performance.md`. Regression coverage asserts throughput, not wall time: `crates/lvu-view/tests/scan_throughput.rs`. On a busy box that p99 reads 1.6–3.0s while the p50 barely moves: with 24 samples it is the maximum, and every one of the six literals driven by hand costs 0.53–0.79s even at load 26–30. |
| **Done** | (W24) `docs/columnar-cache.md`: a derived columnar cache beside the journal, with `raw` as a Binary column. Measured, engine ops only: a cached filter would be about 5x at 620k, 6x at 3M and 3x at 30M, where it becomes I/O-bound; about 0.8 GB of cache per GB of journal, held alongside it. Recommendation is not yet, and the threshold is a usage signal — a source scanned more than once at its generation — not a record count. It also found a cheaper result that needs no cache: today's case-insensitive search lowercases the whole column before searching it at 414 MB/s, where the same search as a `(?i)` regex runs at 1,516 MB/s. One expression in `TextSearch`. |
| **Done** | `mise run janitor -- --stale-flags` reclaims the dependency set a compiler or `RUSTFLAGS` change leaves behind, keyed on the two fingerprint fields no two units of one build can disagree about; skips a target whose newest build used other flags; measured 3.8 → 2.5 GB with zero recompiles after. Also: the janitor never prunes an artifact a fingerprint still references or a target a build holds locked. |
| **Done** | Crate READMEs corrected against the code: `lvu-app` (title dismissal, automatic completion, View mnemonics, nested JSON tree, no schema version, helper on `PYTHONPATH`), `lvu-ingest` (HTTP and restart policies exist; the manager refuses only unasked restores), `lvu-discovery` and `lvu-memory` (stale paragraphs removed), `lvu-core` (claim registry plus record lock), `lvu-view` (manifest carries the view id). |
| **Done** | Docs and repo gardening: 72 files inventoried (docs/gardening-2026-09-08.md); 10 deleted (stale handoff, two capture files, four per-crate reports, an unloaded fixture, twelve accidental zero-byte journal files), 3 merged (module partition → component model, command chain → command enrichment, dialog design → dialog system), previews doc and index rewritten, ledger split; the 🧠 glyph names a control only. |
| **Done** | Stale binaries in the matrix: guarded rather than explained. `matrix:preflight` refuses a binary older than its sources and names the newer input; the matrix header prints the target and binary ages; the likeliest cause was the build and the run disagreeing about `CARGO_TARGET_DIR`. |
| **Done** | The flaky `snapshot_packs_many_evaluation_batches` test framed its captures mid-line; fixed by framing on line boundaries (W13, `f9780b2`). |
| **Done** | Soak and slow-terminal harnesses: `mise run soak`, `soak:long`, `soak:slow-terminal`. Multi-cycle runs complete: six cycles over 620k records and a restart, 36 timed filters, clean shutdowns. Memory answers the leak question — after warm-up the steady state is 103.7, 95.0, 92.1, 92.1, 92.2 MiB, journal 109.0 to 109.9 MiB (the chatty command source appending), derived index 24.0 to 24.3 MiB; nothing climbs. Shutdown 0.445s worst, slowest input-loop iteration 0.082s. |

### preview 052 (2026-09-08, `ba793af`)

| Status | Request |
| --- | --- |
| **Done** | Sixteen-colour terminals: identities map by hue onto the six usable ANSI colours with bold as a second axis, levels and JSON kinds take their ANSI names, the contrast floor is measured against xterm's palette, and Details matches through the shared styling functions. Chrome too: selection, default-button fill, borders and mnemonics resolve by depth in the theme; the default fill was white on cyan at 2.06:1 on sixteen colours and is now black on cyan. |
| **Done** | Misdiagnosis corrected: a restarted source resumes from its durable cursor and repeats nothing (8,192 of 8,192, 8/8 runs with fragments off). The 0–2 extra records are partial-line fragments captured before their terminator, which an uninterrupted run produces just as often. Stated in AGENTS.md; pinned from both sides in `restart_replay_bound.rs`. |
| **Done** | The Details pane colours a record exactly as the log pane does: the token→colour rule exists once (`json_kind_style`), the raw line and flat values go through the log's text styler, and rows inherit level and colour-field styling through `record_style`. |
| **Done** | Editors no longer type the bare character of an unbound Alt-chord (all nine text-field components share one predicate); the loop probe reports dispatch time; two PTY stories that sent Esc and a key back to back (read as Alt-key) now wait for the layer to close. |
| **Done** | The search-race timeout was the story, not the app: it sent Esc and `t` back to back, which a terminal reads as Alt-t, so the Esc never closed the editor. The Time dialog opens in 10–66 ms and reads nothing blocking. 0 failures in 40 runs under load, against 6 before. Same hazard being fixed in the enrichment command story, plus editors ignoring unbound Alt-chords. (W15) |
| **Done** | The janitor reclaims a worktree's cargo target once the worktree is gone, or its branch is merged and the target untouched for six hours; `disk:check` covers the build volume. sccache capped at 10 G. |
| **Done** | Folding no longer blanks the pane: on a 4.8M-row capture every frame was blank for 180 s, now none. The fold feed starts at the visible window instead of position 0, stops evicting the viewport's row cache, and the last usable viewport is kept with `fold: N runs, M hidden, folding K more` in the status. Gutter indicators for folded extent are next (W22). |
| **Done** | Discoverability rule §8.10: Alt mnemonics underlined on their buttons, the palette is the exhaustive index with the chord for the current focus, Help indexes base-screen keys once under Conventions, routine key reminders and all footers removed. Found three palette chords that were never bound and four base operations with no palette row. Audit in docs/dialog-discoverability.md. |
| **Done** | Every saved recipe revision carries its date (additive, no schema bump, undated revisions serialise byte-identically); the Recipes list shows it in §12.9's date column in the display zone. |
| **Done** | Let a *text* enriched column be declared as the event-time basis. `TimeFieldSelection` now carries the chrono format as a fifth token part (four-part tokens still parse), so `ColumnTimeInterpretation::Text` round-trips. The format is inferred from the bounded sample the dialog already reads — RFC 3339, ISO with and without a zone, Apache/CLF — measured by the parser that will actually run, and shown in the confirmation step with its sample and match rate as an editable field: `FormatRequired` is a suggestion, not a refusal. Values the format cannot read become nulls counted through `event_time_invalid`, never dropped. Syslog is recognised and explained instead of offered: it carries no year and a Polars format cannot supply one. |
| **Done** | Command steps are ordered steps of the enrichment chain: interleaved with expressions, a later step reads `<name>.<field>`, saving never runs a program (review then confirm), unrun columns are typed null with the stale state named, recipes carry the step and report a missing program at apply time. Old single-command views migrate into the chain. |
| **Done** | Nested JSON is a tree in Details and Fields (collapse to `{3 keys}`/`[12]`, expand in place, bytes untouched). Fields has a Value pane: inferred type with share and sample, present/distinct counts, top five values, min to max, and one-key Pin/Filter/Exclude/Color/Fold/Correlate. The editors' completion offers nested paths so a path is never typed. Nested filtering is lexical until Polars JSON-path lands (W21, in flight). |

### preview 051 (2026-09-07, `1acd9b3`)

| Status | Request |
| --- | --- |
| **Done** | Every dialog has one default action, filled in the accent colour, run by Enter from anywhere except inside a prose field (Ctrl-Enter there), an expression field (applies), or a control that consumes Enter. Enrichment: Add when empty, Edit with a step selected. Lists open on the relevant or first row. Audit in docs/dialog-default-actions.md. Found View's Apply button vanishing at 80x24. |
| **Done** | The Add source dialog no longer jumps while typing a path: it moved 4 rows and resized by 8 twice per keystroke. Rule §5.2.1: a live region (completion list, candidates, preview) gets a fixed row count when the dialog opens; overflow scrolls, underflow stays blank with its heading. Step editor fixed under the same rule. |
| **Done** | `lvu-discovery` docker tests flaked under fork pressure: a sibling test's fork inherited a fixture still open for writing (`Text file busy`). Every fixture-writing or forking test now holds the lifecycle lock; 0/80 failures where before 2/78. The product had reported the OS error correctly. |
| **Done** | The "7 s to quit under load" was a test race, not the app: the slowest input-loop iteration is 40 ms at load 12 and quit takes 10 ms; the story's `q` landed in a surface Escape had not yet closed. Storage, settings and command settles now shut down concurrently (8 s worst case → 3 s). |
| **Done** | Dataset-relative time ranges (first-to-last, last N of data, ± around the selected record) labelled by which clock they mean; `{`/`}` jump to the record after the next quiet gap, threshold per view in the Time dialog. |

### preview 050 (2026-09-07, `fa6c47a`)

| Status | Request |
| --- | --- |
| **Done** | Command sources no longer outlive the app: killed with it on quit, SIGTERM and SIGKILL (parent-death signal plus process-group kill), the PTY harness reaps the app's session on every exit path, and the janitor sweeps orphans older than ten minutes. Found nine more orphans from the source-control suite on the way. |
| **Done** | PTY matrix scheduled for a shared machine: longest suites first, spaced starts, the three contention-sensitive suites serialised; 51/51 at load 14 where the old runner got 49/51 at load 9, in 100 s. Summary prints per-suite wall time and load. |

### preview 049 (2026-09-07, `c5d3fbd`)

| Status | Request |
| --- | --- |
| **Done** | Escape on a search editor no longer discards a filter you already applied on All events. Opening a saved investigation returns to its transcript. PTY matrix 50/50 twice on the same tree. |
| **Done** | Adopt the dialog anatomy in every dialog: Search, Advanced, Grouping, Time, Enrichment, External command, Add source, View, Recipes, Bookmarks, Fields, Raw context, Storage, Settings, Help, palette, Ask, Investigation. |
| **Done** | Offer `[ Go to ]` in Bookmarks. Honest since bookmarks became source-scoped: it selects the record in its source's All events view, where it is always present. |

### preview 048 (2026-09-07, `62d8630`)

| Status | Request |
| --- | --- |
| **Done** | Every dialog (18 of 18) is on the shared anatomy and size classes: Investigation split into Question, provenance and a `New │ Saved` pane; palette columns fixed and right-aligned; External command on labelled fields with a results pane. — Earlier entry: Dialogs size to their content; no dead rows at 54x16 across every adopted dialog. |
| **Done** | An unaccountable derived-index cache total no longer refuses new sources: rows are served and the view status says `index cache total unverified`. PTY runs no longer leak indexes into `~/.cache/lvu`; the janitor reclaims orphaned ones. |
| **Done** | Forked views appear directly after their origin, and `[`/`]` cycling or a sidebar click counts as the last-used view on restart. |
| **Done** | Ask 🧠 dialog rebuilt on the anatomy: multiline Request scrolls vertically; a prepared task states what it will do with no kind selector. |
| **Done** | Bridge failures now say what actually failed: resources missing, unbuilt, launcher absent, daemon unreachable, or provider unauthenticated. |
| **Done** | A transient derived-index lock no longer kills the source worker; it retries with jittered backoff and the pane reports the wait. |
| **Done** | Discovery's bounded procfs scan gives up under process pressure, returns 0 candidates and reports `file descriptor limit reached` — which is its own budget, not the system limit (1M, 4.3k in use). Misleading on a busy machine. |
| **Done** | `test_discovery_contrast_pty` depends on discovery finding candidates, so it fails when the machine is loaded. Make it tolerate a limited scan. |
| **Done** | Every source has a permanent All events view; applying a filter creates one editable view; restart reopens your last-used view. |
| **Done** | Bookmarks are source-scoped and jump to the record in All events; raw context remains available. |
| **Done** | Load-dependent PTY failures explained: pyte lacked DEC 2026 support (harness), and a migrated workspace DB left by a reverted commit poisoned every suite without its own capture root. Matrix is 46/46. |
| **Done** | Intermittent empty reopen was the derived-index lock defect; fixed with a deterministic regression. |
| **Done** | The Time-test shutdown failure was the same poisoned-workspace nonzero exit; fixed. |
| **Done** | Field correlation across sources: `r` in Fields resolves the record's typed value, maps each source's own field name explicitly, and opens a merged view of every matching record with the correlating fields pinned. Bounded cancellable lookup, persisted across restart. |
| **Done** | Fall back gracefully on 256-colour terminals: without a `COLORTERM` truecolor claim, identity colours are chosen from the xterm colour cube and checked for contrast there, so what lvu measured is what the terminal displays. The 16-colour case is not covered — the cube is assumed available. |
| **Done** | Repeated-pattern folding, off by default, wired into the viewer with editor completion, recipe suggestion and step preview reading the unfolded page. |

### preview 047 (2026-09-07, `2921258`)

| Status | Request |
| --- | --- |
| **Done** | lvu emitted a bare `\x1b[2J` outside the synchronized-output block on resize, so a real terminal flashed the whole screen. The resize clear now happens inside the same synchronized update as the frame that repaints it. |
| **Done** | Enrichment persists an unfinished *edit* draft across restart, alongside an unfinished new step. |
| **Done** | Explain raw-row loading/index failures instead of showing only “query ready.” Wired into the status line; the API alone had shipped with zero callers. |
| **Done** | Fixed the hang where a resize immediately followed by a keypress wedged the app — process alive, terminal left in the alternate screen. crossterm loops `read(2)` until its parser yields an event and stops only on `WouldBlock`, which the blocking standard input never reports, so a read carrying only the start of an escape sequence slept inside `event::poll` with the poll timeout unexpired and no frame or restoration possible. lvu now hands crossterm a private non-blocking view of the terminal. Not dialog-specific; this was the earlier Settings resize/Enter observation. |
