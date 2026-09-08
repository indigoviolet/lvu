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
| **Working** | Field statistics over the whole view, not only the first 2,048 records: the query host aggregates present/distinct/top/min/max for one column path over the membership, the Value pane shows the sample instantly then the whole-view figures with their heading; type naming stays in the app. (W24, after incremental refresh) |
| **Working** | One place to see every operation applied to a view: a read-only summary in evaluation order (sources, role, filter, advanced, grouping, time window and basis, enrichment chain, fold, colour rules, pinned columns, readiness), Enter on a row opens the owning dialog. (W28, Fable) |
| **Working** | The Fields dialog's Fold button is one-way; it should read Unfold when the view is already folded by that column and switch the key when folded by another. (W22) |
| **Working** | Retire the Raw context dialog: `o` jumps to the record in its source's All events view with the full viewer, `o` again returns to the filtered view at the same record; Fields and Bookmarks reopen on return. Decided by the user: "i'd rather not have another dialog which is a view." Design in docs/raw-context-as-jump.md. (W21) |
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
| **Working** | Help (`?`) should not list command-line options; `lvu --help` and the README own the CLI. Remove the `--fresh`/`--resume` lines and say so in the Help rule. (W27) |
| **Working** | In the Fields dialog `x`, `F`, `d` do nothing: the underlined letters are Alt mnemonics only. Rule change: with no text field focused the bare letter activates the button too; Alt always works; implemented once in the shared action row. (W27) |
| **Working** | The startup heart art renders as flat grey and red blocks at the pane's bottom-left when the `/` search editor is open: the scrim flattens the half-block glyphs' fg/bg contrast and the art's anchor moves. Scrim must preserve block-cell shapes and the anchor. (W18) |
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
| **Working** | (W22) A 512 MB source shows an empty log viewport for at least fifteen minutes while capture runs, and the status line advertises the rows it is not drawing: `FOLLOW | raw view | 473050-473088/473088` over a blank pane, with the source pane reporting 1,368,935 records captured. This is "display raw data before derived data is ready" failing at scale, and it is the same shape as the blank-pane defects fixed earlier, now during first capture rather than after a restart. Reproduce: `mise run soak -- --mode long --bytes 536870912`. Also reproduces in short mode on a saturated disk: 349,165 records captured, `98522-98560/98560` over a blank pane, and the soak's own wait for first rows timing out at 900s. |
| **Working** | (W22) `RowReadiness::RowsPending` and `Indexing` are deliberately kept out of the status line as transient. At 512 MB they are not transient: the pane above is blank for minutes with nothing said. Revisit that exclusion once the blank viewport itself is understood — an explanation is not a substitute for the rows, but silence is worse than either. |
| **Done** | (W24) Historical filter latency. The earlier reading — that filtering blocks per batch and uses 2% of a core — did not survive measurement: sampling the app's own `/proc/<pid>/stat` across a filter showed 15.5–21.6s of CPU across 11.1–11.8s of wall clock. It was compute-bound in unoptimised dependency code, over 256-record pages, allocating a UUID string per scanned row twice and hashing every scanned record. The same 620k capture now filters in 0.71–0.81s at 705k–838k records per CPU-second, a 15x wall and 21x CPU-normalised improvement. `mise run soak` p50 10.75s to 0.727s and p99 11.05s to 0.824s on a quiet box, at 1.10 CPU-seconds per query-second, which clears the soak's own one-second query budget. Phase table, causes and what bounds it now: `docs/performance.md`. Regression coverage asserts throughput, not wall time: `crates/lvu-view/tests/scan_throughput.rs`. On a busy box that p99 reads 1.6–3.0s while the p50 barely moves: with 24 samples it is the maximum, and every one of the six literals driven by hand costs 0.53–0.79s even at load 26–30. |
| **Open** | Capture is slow, and it is not the filter defect's cause. Two of its tokio workers sit in `jbd2_log_wait_commit` while the process uses about 5% of one core, so it waits on `Journal::sync_data` — every `sync_every_batches` (8) batches of 64 records, and again per 256 KiB file checkpoint together with a durable cursor store — on a build volume several agents write to. Observed rate varies with that contention by more than two orders of magnitude on the same binary and input (about 390 KB/s of journal on a quiet box, 0.9 KB/s with `fsync` p50 at 20 ms and a 5.4 s worst case). The journal being half again the source is arithmetic, not a defect: a frame is a 16-byte header plus 58 fixed body bytes, which is 1.68x over the generator's ~109-byte records. Open work is the sync policy and the writer thread serving every page read between appends, not the format. Evidence: `mise run soak` sample lines and per-thread `wchan`. |
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
