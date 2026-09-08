# lvu checklist

Current app: **preview 053**. This is the single feedback and work list.

**Done** = available now. **Ready** = tested, awaiting release.
**Working** = being implemented or validated. **Open** = unfinished.
**Decision** = proposal awaiting your decision. Evidence belongs in the [work ledger](docs/work-ledger.md), not this list.

## Current app: preview 052

What a user of the current build needs to know: what is being worked on, what is unfinished, and what waits on a decision, by area. Wording is unchanged from when each row was written.

### Viewing

| Status | Request |
| --- | --- |
| **Working** | Retire the Raw context dialog: `o` jumps to the record in its source's All events view with the full viewer, `o` again returns to the filtered view at the same record; Fields and Bookmarks reopen on return. Decided by the user: "i'd rather not have another dialog which is a view." Design in docs/raw-context-as-jump.md. (W21) |
| **Working** | The Fields dialog's Fold button is one-way; it should read Unfold when the view is already folded by that column and switch the key when folded by another. (W22) |
| **Working** | One place to see every operation applied to a view: a read-only summary in evaluation order (sources, role, filter, advanced, grouping, time window and basis, enrichment chain, fold, colour rules, pinned columns, readiness), Enter on a row opens the owning dialog. (W28, Fable) |
| **Working** | Field statistics over the whole view, not only the first 2,048 records: the query host aggregates present/distinct/top/min/max for one column path over the membership, the Value pane shows the sample instantly then the whole-view figures with their heading; type naming stays in the app. (W24, after incremental refresh) |
| **Working** | A merged multi-source view concatenates by source instead of interleaving by time. Designed in `docs/merged-view-ordering.md`: a k-way merge over the per-record basis times `SourceMatches.times` already carries, materialised once per publication, with eight invariants stated (per-source order preserved, untimed records keep their place, `index_of_id` stays O(k + log n), fold runs contiguous in merged order, identities stable). `crates/lvu-view/tests/merged_ordering.rs` has three live tests pinning what must not break and five `#[ignore]`d ones that fail today and are un-ignored by the implementation. Implementation waits for W24: the order is built in the publication path `perf/query-latency` is changing. |
| **Working** | Add predicate color rules and regex span highlighting. (W18) |

### Filtering and enrichment

| Status | Request |
| --- | --- |
| **Working** | Search (`/`) and Advanced (`p`) are independent AND-ed constraints with separate dialogs and nothing showing which are active. One Filter dialog with `Search │ Advanced` tabs, each tab with its own draft and applied state, the header naming the active constraints; both stay allowed; `p` is retired, `/` is the only key and the Advanced tab is reached inside the dialog. (W29, Fable) |
| **Working** | Saving a new enrichment step on a large source gives no feedback: the step is added but the editor stays open silently, and a second Enter reports a duplicate field. Save must show Evaluating, block a second Save, close to the list on acceptance and stay with the error on rejection. (W19) |

### Folding

| Status | Request |
| --- | --- |
| **Working** | Folding by a single column: the fold key is any column, defaulting to a derived pattern column (normalised text + level, today's behaviour); choosing fields is just an enrichment column built from them, and no normalisation applies to fields not included. A Folding dialog sets key column, minimum run, scope and normalisation per view. (W23) |

### Time

| Status | Request |
| --- | --- |
| **Working** | Explicit time display and sort: order shown as capture (arrival) in the Time dialog and status line; display zone in Settings, default UTC, shown read-only beside the basis; fixed offsets only, no DST. (W19) |

### Dialogs

| Status | Request |
| --- | --- |
| **Working** | The startup heart art renders as flat grey and red blocks at the pane's bottom-left when the `/` search editor is open: the scrim flattens the half-block glyphs' fg/bg contrast and the art's anchor moves. Scrim must preserve block-cell shapes and the anchor. (W18) |
| **Working** | In the Fields dialog `x`, `F`, `d` do nothing: the underlined letters are Alt mnemonics only. Rule change: with no text field focused the bare letter activates the button too; Alt always works; implemented once in the shared action row. (W27) |
| **Working** | Help (`?`) should not list command-line options; `lvu --help` and the README own the CLI. Remove the `--fresh`/`--resume` lines and say so in the Help rule. (W27) |
| **Working** | Convert dialogs to owned components (docs/component-model.md). Done: 14 of 16 dialogs plus Folding and the fork subsystem inside `Views` (`Action` 194→~80, `Focus` 22→8, `app.rs` 13.9k→8.8k, `ui.rs` 8.2k→4.0k lines). In flight: Ask/Investigation (W14). Raw context held pending the `o` decision. |
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

### Sources and capture

| Status | Request |
| --- | --- |
| **Done** | `lvu` with no arguments resumes last session's sources through the sidebar's Restart path: files from the durable cursor, commands with their recorded cwd and environment, a status notice with the counts; `--fresh` acquires nothing and deletes nothing; arguments add to the resumed set; `--resume` is the explicit default. Session record is a plain file beside the workspace database, no migration. |
| **Done** | Fixed: a forked child inherited the journal's flock through the pre-exec window, so a stop-and-restart inside it was refused (11 of 20 runs under fork pressure, 0 without). Ownership now uses an in-process claim registry plus a POSIX record lock that fork does not pass on; a storage probe that would have destroyed the lock it checked was fixed too; the reproducer is in the gate. Earlier: Restarting a source under load fails about one run in three with `Journal(AlreadyOpen)`: the lock holder is the same process, so some in-process handle outlives a stop that reported complete. Reproducer in `crates/lvu-ingest/tests/restart_race.rs`; W13 root-causing with wall-clock sampling. No retry: a retry would turn a leaked handle into a stall. — Earlier entry: Restarting a source right after `stop()` reports `complete` fails with `Journal(AlreadyOpen)` under load: the journal lock is still held. Reproduced twice — `live_view.rs:3310` in a full-workspace run, and `crates/lvu-ingest/tests/restart_race.rs` at attempt 62 of 200. A probe naming the lock holder shows it is *this* process (`self=494583`, `pid 494583 fd 11`), so it is neither cross-process contention nor descriptor inheritance; some in-process handle outlives the stop that reported complete. Two hundred cycles at rest pass, a forking neighbour does not reproduce it, and lifecycle probes perturb the timing enough to hide it. Next: sample `run_writer` entry/return and the stop reply against a wall clock rather than printing, so the ordering survives instrumentation. |
| **Working** | Match file suggestions using space-separated AND terms, glob patterns, and bounded cancellable `**` recursion with clear partial results. |
| **Working** | Fix Source typing losing literal `q` while automatic suggestions are scanning or visible; preserve Escape completion-first dismissal. |
| **Working** | Let multiple lvu windows automatically share a background capture worker; independent views, detach on close, stop after the last window. |

### Assistance

| Status | Request |
| --- | --- |
| **Open** | The three AI shutdown settles (investigation, source-ai, ai) are still serial and add 9 s to the worst-case shutdown bound; they share `self.agent` and need their own assignment with the AI suites as the gate. |
| **Open** | Let short AI requests inspect more data when the bounded prompt sample is insufficient. |

### Performance

| Status | Request |
| --- | --- |
| **Open** | Capture is slow, and it is not the filter defect's cause. Two of its tokio workers sit in `jbd2_log_wait_commit` while the process uses about 5% of one core, so it waits on `Journal::sync_data` — every `sync_every_batches` (8) batches of 64 records, and again per 256 KiB file checkpoint together with a durable cursor store — on a build volume several agents write to. Observed rate varies with that contention by more than two orders of magnitude on the same binary and input (about 390 KB/s of journal on a quiet box, 0.9 KB/s with `fsync` p50 at 20 ms and a 5.4 s worst case). The journal being half again the source is arithmetic, not a defect: a frame is a 16-byte header plus 58 fixed body bytes, which is 1.68x over the generator's ~109-byte records. Open work is the sync policy and the writer thread serving every page read between appends, not the format. Evidence: `mise run soak` sample lines and per-thread `wchan`. |
| **Done** | (W24) Historical filter latency. The earlier reading — that filtering blocks per batch and uses 2% of a core — did not survive measurement: sampling the app's own `/proc/<pid>/stat` across a filter showed 15.5–21.6s of CPU across 11.1–11.8s of wall clock. It was compute-bound in unoptimised dependency code, over 256-record pages, allocating a UUID string per scanned row twice and hashing every scanned record. The same 620k capture now filters in 0.71–0.81s at 705k–838k records per CPU-second, a 15x wall and 21x CPU-normalised improvement. `mise run soak` p50 10.75s to 0.727s and p99 11.05s to 0.824s on a quiet box, at 1.10 CPU-seconds per query-second, which clears the soak's own one-second query budget. Phase table, causes and what bounds it now: `docs/performance.md`. Regression coverage asserts throughput, not wall time: `crates/lvu-view/tests/scan_throughput.rs`. On a busy box that p99 reads 1.6–3.0s while the p50 barely moves: with 24 samples it is the maximum, and every one of the six literals driven by hand costs 0.53–0.79s even at load 26–30. |
| **Working** | (W22) A 512 MB source shows an empty log viewport for at least fifteen minutes while capture runs, and the status line advertises the rows it is not drawing: `FOLLOW | raw view | 473050-473088/473088` over a blank pane, with the source pane reporting 1,368,935 records captured. This is "display raw data before derived data is ready" failing at scale, and it is the same shape as the blank-pane defects fixed earlier, now during first capture rather than after a restart. Reproduce: `mise run soak -- --mode long --bytes 536870912`. Also reproduces in short mode on a saturated disk: 349,165 records captured, `98522-98560/98560` over a blank pane, and the soak's own wait for first rows timing out at 900s. |
| **Open** | Short design note only, last in W24's queue, no code: a derived, disposable columnar cache (Arrow IPC or Parquet per journal segment) beside the raw-bytes journal, with `raw` itself as a binary column so substring search scans one contiguous buffer, with invalidation, expected gain at 620k/3M/30M records, disk cost and the threshold at which it pays. docs/columnar-cache.md. (W24, after its queue) |
| **Working** | (W22) A 512 MB source shows an empty log viewport for at least fifteen minutes while capture runs, and the status line advertises the rows it is not drawing: `FOLLOW | raw view | 473050-473088/473088` over a blank pane, with the source pane reporting 1,368,935 records captured. This is "display raw data before derived data is ready" failing at scale, and it is the same shape as the blank-pane defects fixed earlier, now during first capture rather than after a restart. Reproduce: `mise run soak -- --mode long --bytes 536870912`. |
| **Working** | (W22) `RowReadiness::RowsPending` and `Indexing` are deliberately kept out of the status line as transient. At 512 MB they are not transient: the pane above is blank for minutes with nothing said. Revisit that exclusion once the blank viewport itself is understood — an explanation is not a substitute for the rows, but silence is worse than either. |

### Tooling and release

| Status | Request |
| --- | --- |
| **Open** | `test_empty_event_fields_pty.py` loses its 3 s wait for `Fields closed` at load 15 or so, independent of any recent change: interleaved runs of the same suite against the pre-fix and post-fix binaries under one load failed 1 in 8 each. It needs the treatment the other waits got — a bound tied to something the app actually signals, not a wider number. |
| **Working** | First public release `v0.1.0`: W26 validates archives, formula and mise install, dry-runs the release workflow, then tags main, publishes the release, and pushes the formula to the existing `indigoviolet/homebrew-tap`. macOS archives ship tentatively, marked untested, for the user to try. (priority) |
| **Open** | After a Cargo feature change (Polars `extract_jsonpath`) the primary target served stale `lvu`/`lvu-app` binaries under sccache: two PTY suites failed deterministically until `cargo clean -p lvu -p lvu-app`. Every agent's gate builds the same way; find whether sccache or incremental fingerprints are at fault and make the matrix preflight detect a binary older than its sources. |
| **Open** | Validate installation/terminal/process behavior on macOS and Windows. Audit done (docs/portability.md); no macOS/Windows run yet. |
| **Open** | Explain the flaky `snapshot_packs_many_evaluation_batches` lvu-view test; it fails ~1 run in 3 under parallel load on baseline. |
| **Ready** | Soak and slow-terminal harnesses: `mise run soak`, `soak:long`, `soak:slow-terminal`. Multi-cycle runs complete: six cycles over 620k records and a restart, 36 timed filters, clean shutdowns. Memory answers the leak question — after warm-up the steady state is 103.7, 95.0, 92.1, 92.1, 92.2 MiB, journal 109.0 to 109.9 MiB (the chatty command source appending), derived index 24.0 to 24.3 MiB; nothing climbs. Shutdown 0.445s worst, slowest input-loop iteration 0.082s. |

## Shipped since preview 047

Done rows by the preview that first carried them (the publication commit after the row was marked Done). Older Done rows are in the [work ledger](docs/work-ledger.md) under `2026-09-08 — checklist rows shipped before preview 047`.

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
