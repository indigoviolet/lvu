# Command enrichment

Published in **preview034**. The editor, frozen input reader, durable runner,
controller and read-only result presentation passed combined Rust,
SQLite/subprocess and copied-app PTY acceptance. Preview033 does not include it.

## Execution boundary

A command is a step of the view's enrichment chain (docs/dialog-system.md
§8.14). It sits anywhere in the order, between expression steps or beside
other command steps, and has an output name — its **prefix** — that later
steps, filters and searches read its results under, as `<name>.<field>`
columns. It uses an executable and separate arguments, an optional working
directory, and environment overrides. It is not shell text. Restart policy is
`Never`.

A command's input is the steps before it: the raw record and every expression
and command output that precedes it in the chain, never its own output or a
later step's. A step may only read a command that comes before it, and the
chain is rejected otherwise, keeping the accepted chain and naming the step
to move.

Saving a definition is a chain change through the same query seam an
expression step uses, and is separate from running it. A run first prepares
a fixed snapshot of the accepted view as the command's input, then presents
the executable, arguments, working directory, environment keys and
source/record count for confirmation. Later arrivals are outside that
snapshot. The editor controls are Ctrl-S to save, Ctrl-R to prepare the
review, Enter to confirm it, and Escape to close or cancel execution. Once
result-save admission succeeds, the state becomes Saving results: Escape only
closes the dialog and cannot cancel that accepted save. A matching save
acknowledgement makes the results visible; a save failure retains the previous
publication. Arguments and environment entries use separate lines; Alt-N adds
a line.

The first application slice accepts at most 1,024 records and 4 MiB of input.
Preparing input also caps scanned records at 100,000 and scanned raw bytes at
256 MiB. Exceeding a limit fails before command delivery; it never selects a
sample silently. Narrow the view and prepare another review. Unsupported typed
values and failed native replay also stop preparation.
The scan bound applies to source replay, so narrowing a filter alone may not
bring a large capture within it; reducing the source set or using a smaller
capture may be necessary.
The runner requires protocol completion within 10 seconds, limits each output
frame to 1 MiB and bounds total subprocess output at 8 MiB. Durable result
admission remains stricter at 1 MiB for the complete result set.

## Protocol and results

Commands must implement the [JSON Lines protocol](contracts.md#command-enrichment-json-lines),
including the final matching `batch_complete` frame. An ordinary program that
prints unrelated lines is not a compatible enrichment command.

Input has stable record IDs, a display `raw` string and typed parsed/native fields.
The display string may replace invalid UTF-8; the exact original bytes, delimiters
and physical identity remain in the durable capture. Outputs must be additive:
they cannot overwrite input fields, `raw`, or reserved metadata. Replies may arrive
out of order because results are joined by stable record ID.

Results are presented in two places, from the same published rows:

- **Details** is read-only, with `<name>.<field>` entries and an explicit
  `<name>.status` of `Ready · last explicit run` or `Pending — run
  explicitly` per record. Alt-PgUp/PgDn or the mouse wheel scrolls Details;
  Alt-Home returns to the top. Selecting another record resets its scroll.
  Raw context stays raw.
- **The evaluation frame.** Published rows are joined into every batch the
  view evaluates as `<name>.<field>` columns, typed from the values (integer,
  float, boolean, else text; structured values as their JSON text), so later
  expression steps, the advanced filter and expression searches read them
  like any other column. A record the command has not answered reads as
  null. A command step that has never been run contributes null columns of
  no particular type: a step that cannot evaluate over null carries the
  diagnostic `waits for a command step that has not run` and reads as null,
  a filter over them is accepted but not applied yet (the status line says
  `filter waits for a command step that has not run`), and neither rejects
  the chain. A waiting filter is not applied because it would hide the rows
  the command needs as its input. When a publication lands or a saved one is
  restored, the view's chain is re-evaluated over it as one accepted query;
  until then the applied view is unchanged.

A command's input is the accepted view's records, so a filter that reads a
command's published output narrows that command's next input too. To run a
command over every record again, clear the filter first; the review shows
the record count before anything runs.

Display grouping, pins, snapshots and agent inspection do not consume command
results. A result set is limited to 1 MiB; active command presentation across
the workspace is limited to 8 MiB, counted per view and step. Admission
failure retains previous results.

## Durability and failure

Attempts are reserved durably before possible payload delivery. Their scope binds
the view, the command step, the command definition and the chain prefix it
reads (every earlier step's id, source and, for a command step, its whole
definition); a changing live-data revision does not grant another attempt.
An explicit later run reuses Ready records and delivers only never-attempted
records. A reserved or failed record with no usable result is not implicitly
retried. Capacity is 100,000 attempts per scope, with no automatic eviction.

A globally invalid batch, including replies without a valid completion frame,
cannot become successful by pressing Run again: newly reserved records finalize
as Failed. A valid completed batch can preserve independent successful records
alongside event-level failures. Previously finalized results are immutable.

Each command step keeps a definition revision and, separately, the reference
to its last published result set. Editing a command must not destroy the old
result set: the step is `Unrun` again, and the old results stay readable until
a run replaces them. Removing the step drops them. Restoring a definition or
published result never launches the command; a cloned view keeps the
definitions and none of the results. A crash after reservation but before
durable finalization leaves attempted records unavailable; cancellation does
not undo an attempt that may already have been delivered.

## Recipes

A recipe stores a command step as its program, arguments, working directory,
environment and name, in its place in the chain. It never stores results.
Applying a recipe installs the step unrun; a recipe that names a program the
machine cannot start (not an executable file at that path, or not on `PATH`
for a bare name) is applied all the same and says so at once:
`recipe applied · not on this machine: geo needs /opt/geo · the step is saved
unrun`. Shell text is never accepted in a recipe's command step.

## Storage

Workspace schema v4 (preview 034) added the attempts and results tables; a
build older than that refuses a migrated database. The workspace schema has
moved on since (v6 at preview 048); the table of versions is in
[`previews.md`](previews.md).

The stored chain (`enrichment_chain`) carries a command step as a step with
its definition; each step's revision and publication reference live in
`command_steps`, keyed by stage id. A view saved by a build with the single
command slot (`command_enrichment`, `command_enrichment_revision`,
`command_publication`) is read once and migrated: the slot becomes the last
step of the chain, named `command`, with its revision and publication intact;
the slot is never written again.

## Decisions behind the chain model

Command steps became ordered steps of the chain (rule §8.14) in preview 052.
Before that, one command per view sat in a slot of its own, always ran after
every expression step, its results were read-only in Details as
`command.<field>`, recipes refused a view with a command, and the attempt
scope hashed the whole chain so any expression step fenced the command. The
invariant audit of the change and its per-seam mechanics were recorded in
`command-chain.md` (git history, `14ad91c`). What remains the user's:

1. **Re-run is confirmed, never automatic.** A step becomes `Unrun` when its
   definition or the steps before it change; the list names it, Details says
   `Pending`, and nothing runs until the user reviews and confirms from the
   dialog. This keeps "saving or restoring never starts this command"
   literally true and keeps the review as the moment the user sees what
   leaves the machine. If automatic re-run is ever wanted, the honest form is
   a per-step opt-in shown in the review, not a default.
2. **`External command…` on a command row opens that step**; on an expression
   row or an empty chain it inserts a new step after the selection. `Edit` on
   a command row also opens it. With only command rows selected there is no
   one-key way to add a second command directly after another: select the
   expression above, or add it anywhere and move it with Alt-Up/Down.
3. **Legacy migration naming.** A view saved with the single slot becomes a
   chain whose last step is named `command`, so its `command.<field>` entries
   keep working. The stage id is `command` unless an expression step already
   uses it.
4. **Placeholder typing before a run.** Unpublished columns are `Null`-typed,
   and steps that still fail over null wait with a diagnostic rather than
   reject the chain. Text-typed placeholders made `pl.col('geo.score') + 1`
   reject a valid chain.
5. **A filter over a command's output narrows that command's next input.**
   The command's input is the accepted view, so after results are published
   a run re-attempts only the rows the filter passed; the review shows the
   count first, and before the first run such a filter is not applied at all.
   Freezing the input with those filters removed would mean a second
   membership per run; worth doing if users hit this, not before.

Not built, on purpose: display grouping over command output (grouping is a
projection over raw bytes and reads no enrichment column), and text search
over command columns (a literal search reads no columns; an expression search
is a filter and behaves as one).
