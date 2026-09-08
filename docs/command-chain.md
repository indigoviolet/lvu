# Command steps in the chain: design, audit and decisions

The rule is `docs/dialog-system.md` §8.14; the behaviour is
`docs/command-enrichment.md`. This note records what changed, what was
audited against the AGENTS.md invariants and the two-layer enrichment UX,
and the decisions that are the user's.

## What was true before

- One command per view, in a slot of its own (`ViewState.command_enrichment`),
  always run after every expression step. Its results were read-only in
  Details as `command.<field>`; no expression, filter, search or recipe could
  read them (`docs/command-enrichment.md`: "Native filtering, grouping, pins
  … do not consume command results").
- Saving the command went to `lvu-app`'s command controller as a durable
  definition write, separate from the chain query seam.
- Recipes refused to save a view with a command (`recipe_command_guard`) and
  refused to apply a recipe with a command stage.
- The attempt scope hashed the whole native chain, so any expression step
  fenced the command, including steps after it.

## The model now

| Concern | Now |
| --- | --- |
| Where a command lives | `EnrichmentDefinition { id, source, command: Option<CommandDefinition> }`; for a command step `source` is its output prefix. One ordered `ViewState.enrichments`. |
| Run state | `ViewState.command_steps: BTreeMap<stage id, CommandStepState { revision, publication }>`. |
| Saving | `PendingEnrichmentMutation::CommandSave` / `Remove` / `Reorder` through `Views::enqueue_enrichment_chain`; the dialog reacts to `ViewEvent::QueryAccepted` / `QueryRejected` for its view. The shell bumps a step's revision when its definition changed or it is new; a restored chain keeps its revisions. |
| Input of a run | `NativeViewAdapter::freeze_input_through(view, Some(stage), limits)`: the steps before it, with the command results published before it. |
| Attempt scope | `(view, stage, command definition, fingerprint of the chain prefix)`; the prefix includes earlier command steps' definitions. |
| Results downstream | `NativeViewAdapter::set_command_results(view, stage, name, rows)` stores rows per view; `command_columns::join_command_columns` adds `<name>.<field>` columns to every batch frame before the chain runs, typed from the values. The app reaffirms the chain (`Views::reaffirm_enrichment_chain`) once a publication lands or is restored, after the view has nothing in flight. |
| Before a run | Unpublished columns an expression, filter or search names are added as `Null`-typed columns. Steps that read an unrun command and fail over null carry `waits for a command step that has not run` and project null; a filter that reads one is accepted but not applied, with the same diagnostic in the status line, so the command keeps its input. Neither rejects the chain. |
| Ordering | `validate_command_order` rejects a chain where a step reads `<name>.*` of a command step after it. |
| Presentation | `CommandPresentation` keyed by `(view, stage)`, configured with each view's command steps in order; Details shows `<name>.status`, `<name>.<field>`, `<name>.diagnostic`. |
| Persistence | `StoredEnrichment.command` and `PresentationState.command_steps` (both `#[serde(default)]`, no schema bump). The legacy slot is read once and migrated to the last step named `command`; never written. |
| Recipes | `StageDefinition::Command { id: String, name, command }` is captured and replayed. `recipe_command_guard` and the incompatibility are gone. `program_available` / `missing_programs` decide the apply-time notice. |
| Shutdown | `settle_pending_queries` gives a chain change submitted just before quitting a bounded moment to be answered before the final autosave. |

## Audit against the invariants

| Invariant | How it holds |
| --- | --- |
| Raw bytes and stable IDs | Command columns are joined by `(source id, sequence)`; `raw` and `_lvu_*` are never touched and an existing record field named `<name>.<field>` wins over the join. |
| Saving or restoring never starts a command | Save is a chain query; restore, clone and recipe apply install steps unrun; only the dialog's reviewed confirmation executes. Tests: `saving_a_command_never_runs_it…`, `a_recipe_with_a_missing_program_applies_unrun_and_says_so`, the command enrichment PTY story (`restore launched saved command` guard). |
| No shell text, `RestartPolicy::Never` | Unchanged in the dialog; the recipe validator now rejects `Shell` and any restart policy for command stages. |
| Bounded memory and queues | Publications keep their 1 MiB / 8 MiB limits per `(view, stage)`; the controller's column-update queue is bounded (256, newest per step wins); `command_results` in `lvu-view` mirrors published rows only, which are already bounded. |
| Shared UI geometry | The list and dialog reuse `render_pane_heading`, `packed_button_rows`, `class_l_popup`; the External command dialog gained one field row. |
| Two-layer enrichment UX | Layer one lists both kinds of step; layer two is the step editor for expressions and the External command dialog for commands, reached by `Edit` on the row. `External command…` stays the way to a command from the list. |
| One coherent commit; TODO/ledger untouched | Yes. |

## What was not built, and why

- **Display grouping over command output.** Grouping is a display projection
  over raw bytes (`ContinuationRule`), not a frame query; it does not read
  enrichment columns either. Left as it is.
- **Automatic re-run.** See decision 1.
- **Text search over command columns.** A literal search does not read
  columns; an expression search is a filter and behaves as one.

## Decisions for the user

1. **Automatic or confirmed re-run when a command's inputs change.**
   Built: confirmed. A command step becomes `Unrun` when its definition or
   the steps before it change, the list names it, Details says `Pending`, and
   nothing runs until the user reviews and confirms from the dialog.
   Options:
   - *Confirmed (built, recommended).* Keeps "saving or restoring never
     starts this command" literally true, keeps the review as the moment the
     user sees what leaves the machine, and never surprises with a program
     run from an edit two steps up the chain.
   - *Automatic re-run of the whole snapshot.* Fewer keystrokes; but a
     definition edit or a reorder would launch a program without a review,
     and a chain of two commands would launch both.
   - *Automatic for new records only.* Would break the fixed-snapshot
     semantics ("later arrivals are outside that snapshot") and the attempt
     scope's meaning.
   Recommendation: confirmed. If automatic is wanted later, the honest form
   is a per-step opt-in shown in the review, not a default.
2. **`External command…` on a command row.** Built: it opens the selected
   command step; on an expression row (or an empty chain) it inserts a new
   step after the selection. `Edit` on a command row also opens it. The
   trade-off: with only command rows selected there is no one-key way to add
   a second command directly after another; select the expression above, or
   add the command after any row and move it with Alt-Up/Down. The
   alternative (`External command…` always inserts) made the palette's and
   the list's long-standing "open my command" gesture create a second step,
   which the command enrichment PTY story caught immediately.
3. **Legacy migration naming.** A view saved with the single slot becomes a
   chain whose last step is named `command`, so its `command.<field>` Details
   entries and any user habits keep working. The stage id is the slot's id
   (`command`) unless an expression step already uses it.
4. **Placeholder typing before a run.** Unpublished columns are `Null`-typed
   (arithmetic and comparisons yield null) and steps that still fail (string
   namespaces) wait with a diagnostic rather than reject the chain. The
   alternative, text-typed placeholders, made `pl.col('geo.score') + 1`
   reject a valid chain.
5. **A filter over a command's output and that command's input.** The
   command's input is the accepted view (as before), so once results are
   published a filter over them narrows the next run's input as well: a run
   after `pl.col('geo.score') > 5` re-attempts only the rows that passed.
   Built that way, documented, and the review shows the count first. The
   alternative, freezing the input with the filters that read the step's own
   or later output removed, means recomputing a second membership per run;
   worth doing if users hit this, not before. Before the first run the
   filter is not applied at all for exactly this reason (rule §8.14).
