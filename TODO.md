# lvu checklist

Current app: **preview 050**. This is the single feedback and work list.

**Done** = available now. **Ready** = tested, awaiting release.
**Working** = being implemented or validated. **Open** = unfinished.
**Decision** = proposal awaiting your decision. Evidence belongs in the [work ledger](docs/work-ledger.md), not this list.

| Status | Request |
| --- | --- |
| **Working** | Folding by a single column: the fold key is any column, defaulting to a derived pattern column (normalised text + level, today's behaviour); choosing fields is just an enrichment column built from them, and no normalisation applies to fields not included. A Folding dialog sets key column, minimum run, scope and normalisation per view. (W23) |
| **Working** | Toggling folding blanked the pane for seconds on a large capture with nothing to say what was happening. Rule: a presentation-only recompute never blanks the pane; last rows stay with a readiness status, the visible window folds first and shows, the rest folds in the background. Other blanking paths to be listed. (W22) |
| **Working** | Every dialog declares a default action, shown as the primary button and executed on Enter (Enrichment: Add when empty, Edit when a step is selected); list dialogs open with the first or most relevant item selected. Rule in the dialog system, audit of all 18 dialogs, applied to component-owned dialogs. (W21, Fable) |
| **Working** | The Source dialog jumps as the file completion list changes size while typing. Fix as a design-system rule: regions that change while typing get a fixed height with scrollbar and overflow count, so dialog geometry never changes between keystrokes; apply to Source and any other live list. (W20) |
| **Done** | `lvu-discovery` docker tests flaked under fork pressure: a sibling test's fork inherited a fixture still open for writing (`Text file busy`). Every fixture-writing or forking test now holds the lifecycle lock; 0/80 failures where before 2/78. The product had reported the OS error correctly. |
| **Done** | Command sources no longer outlive the app: killed with it on quit, SIGTERM and SIGKILL (parent-death signal plus process-group kill), the PTY harness reaps the app's session on every exit path, and the janitor sweeps orphans older than ten minutes. Found nine more orphans from the source-control suite on the way. |
| **Done** | PTY matrix scheduled for a shared machine: longest suites first, spaced starts, the three contention-sensitive suites serialised; 51/51 at load 14 where the old runner got 49/51 at load 9, in 100 s. Summary prints per-suite wall time and load. |
| **Done** | The "7 s to quit under load" was a test race, not the app: the slowest input-loop iteration is 40 ms at load 12 and quit takes 10 ms; the story's `q` landed in a surface Escape had not yet closed. Storage, settings and command settles now shut down concurrently (8 s worst case → 3 s). |
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
| **Working** | Convert dialogs to owned components (docs/component-model.md). Done: 12 of 16 dialogs plus the fork subsystem inside `Views` (`Action` 194→85, `Focus` 22→9, `app.rs` 13.9k→8.5k, `ui.rs` 8.2k→4.0k lines). In flight: Ask/Investigation (W14), Raw context then Bookmarks (W15). |
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
| **Open** | Give `RecipeItem` a saved-at date. §12.9's row ends in a date column; the recipe list carries only a revision id, which currently takes that column. |
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
| **Open** | Let a *text* enriched column be declared as the event-time basis. `lvu_query::ColumnTimeInterpretation::Text` requires an explicit chrono format and `lvu_live::TimeInterpretation::Text` carries none, so a text column cannot round-trip through a field token; only numeric epoch columns are offered today. |
| **Open** | Finish the audit removing duplicate shortcut inventories and routine Enter/Tab/Esc reminders from all dialogs. |
| **Open** | Make every relevant operation discoverable in the app instead of requiring documentation or memorized Alt shortcuts. |
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
| **Open** | A merged multi-source view concatenates by source instead of interleaving by time. Needs event-time ordering in the engine: paging across sources by time, `index_of_id`, fold contiguity, live append inserting mid-stream without moving the selection, stable IDs under reordering. |
| **Done** | Support selecting arbitrary timestamp fields, beyond the accepted `timestamp_utc` enrichment. |
| **Done** | Improve automatic timestamp/epoch recognition without requiring AI. |
| **Done** | Fall back gracefully on 256-colour terminals: without a `COLORTERM` truecolor claim, identity colours are chosen from the xterm colour cube and checked for contrast there, so what lvu measured is what the terminal displays. The 16-colour case is not covered — the cube is assumed available. |
| **Open** | Explain the flaky `snapshot_packs_many_evaluation_batches` lvu-view test; it fails ~1 run in 3 under parallel load on baseline. |
| **Working** | Add predicate color rules and regex span highlighting. (W18) |
| **Open** | Add richer field/type/value exploration and nested JSON expansion. |
| **Done** | Repeated-pattern folding, off by default, wired into the viewer with editor completion, recipe suggestion and step preview reading the unfolded page. |
| **Done** | Add HTTP/reconnect sources and explicit command restart policies. |
| **Done** | Add ownership-aware capture/investigation deletion, retention and cache-pressure handling. |
| **Working** | Validate longer production workloads and latency-impaired remote terminals: W13 building a 20–30 minute soak under `mise run soak` (RSS, index growth, journal size, input-loop and query latency, shutdown) plus a 200 ms RTT fragmenting relay for the terminal stories. |
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
