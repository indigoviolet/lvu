# lvu checklist

Current app: **preview 043**. This is the single feedback and work list.

**Done** = available now. **Ready** = tested, awaiting release.
**Working** = being implemented or validated. **Open** = unfinished.
**Decision** = proposal awaiting your decision. Evidence belongs in the [work ledger](docs/work-ledger.md), not this list.

| Status | Request |
| --- | --- |
| **Working** | Restructure the enrichment dialog into two layers: a step list with Add/Edit/Remove, and a focused step editor showing input, expression and output, saving back to the list. |
| **Working** | Give dialogs more room like the `o` Context dialog: audit every dialog's size, define shared size classes, and apply them for a cleaner, less-clipped layout. |
| **Working** | Fix the Source 🧠 proposal being unreviewable at small terminal sizes: launch/cwd/restart/env/why must be reachable before Start reviewed. |
| **Working** | Fix silent data loss: with a legacy `.lvu-captures` dir in the cwd, the capture root flips between runs and abandons sources, workspace, filters and enrichment. |
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
| **Working** | Investigate blank restored filtered views, including cases where the query reports matches but no rows appear. |
| **Working** | Match file suggestions using space-separated AND terms, glob patterns, and bounded cancellable `**` recursion with clear partial results. |
| **Working** | Fix Source typing losing literal `q` while automatic suggestions are scanning or visible; preserve Escape completion-first dismissal. |
| **Done** | Complete file paths automatically while typing; remove Complete path and Open buttons; Enter opens the selected file or enters a directory. |
| **Done** | Restore Up/Down selection in discovery and file suggestions directly from the search/input field; Enter opens the selected result. |
| **Open** | Finish the audit removing duplicate shortcut inventories and routine Enter/Tab/Esc reminders from all dialogs. |
| **Open** | Make every relevant operation discoverable in the app instead of requiring documentation or memorized Alt shortcuts. |
| **Open** | Provide an immutable **All events** view per source; filtering it creates an editable view. Other views remain editable. |
| **Open** | Make bookmark jumps use All events instead of the confusing raw-context detour. |
| **Open** | Let short AI requests inspect more data when the bounded prompt sample is insufficient. |
| **Working** | Explain raw-row loading/index failures instead of showing only “query ready.” |
| **Open** | Diagnose intermittent empty plain-file reopen. Passing reruns have not established the cause. |
| **Open** | Diagnose the unexplained Time-test shutdown failure. |
| **Open** | Resolve the earlier Settings resize/Enter observation; do not treat passing reruns as an explanation. |
| **Working** | Let multiple lvu windows automatically share a background capture worker; independent views, detach on close, stop after the last window. |
| **Working** | Support installation through Homebrew. |
| **Working** | Support installation through mise, including relocatable helper/runtime resources. |
| **Working** | Validate supported installation, terminal and process behavior on macOS and Windows; Linux is tested today. |
| **Open** | Finish field correlation across sources, including lookup, persistence and actual app integration. |
| **Open** | Extend command enrichment to multiple/interleaved steps, downstream queries and recipes. |
| **Open** | Add dataset-relative time ranges, gap navigation and explicit time display/sort behavior. |
| **Working** | Support selecting arbitrary timestamp fields, beyond the accepted `timestamp_utc` enrichment. |
| **Working** | Improve automatic timestamp/epoch recognition without requiring AI. |
| **Open** | Fall back gracefully on 256/16-colour terminals: value colours emit truecolor RGB with no downgrade path. |
| **Open** | Explain the flaky `snapshot_packs_many_evaluation_batches` lvu-view test; it fails ~1 run in 3 under parallel load on baseline. |
| **Open** | Add predicate color rules and regex span highlighting. |
| **Open** | Add richer field/type/value exploration and nested JSON expansion. |
| **Working** | Add repeated-pattern folding beyond existing multiline grouping. |
| **Working** | Add HTTP/reconnect sources and explicit command restart policies. |
| **Working** | Add ownership-aware capture/investigation deletion, retention and cache-pressure handling. |
| **Open** | Validate longer production workloads and latency-impaired remote terminals. |
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
