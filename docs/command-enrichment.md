# Command enrichment

Published in **preview034**. The editor, frozen input reader, durable runner,
controller and read-only result presentation passed combined Rust,
SQLite/subprocess and copied-app PTY acceptance. Preview033 does not include it.

## Execution boundary

A command is an optional final step after the view's native enrichment steps.
It uses an executable and separate arguments, an optional working directory, and
environment overrides. It is not shell text. Restart policy is `Never`.

Saving a definition is separate from running it. A run first prepares a fixed
snapshot of the accepted view, then presents the executable, arguments, working
directory, environment keys and source/record count for confirmation. Later
arrivals are outside that snapshot. The editor controls are Ctrl-S to
save, Ctrl-R to prepare the review, Enter to confirm it, and Escape to close or
cancel execution. Once result-save admission succeeds, the state becomes Saving
results: Escape only closes the dialog and cannot cancel that accepted save. A
matching save acknowledgement makes the results visible; a save failure retains
the previous publication. Arguments and environment entries use separate lines; Alt-N adds a line.

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

Result presentation is read-only in Details, with `command.<field>` entries and an
explicit Ready or Pending status. Alt-PgUp/PgDn or the mouse wheel scrolls
Details; Alt-Home returns to the top. Selecting another record resets its scroll.
Raw context stays raw. Native filtering,
grouping, pins, snapshots and agent inspection do not consume command results in
this slice. A result set is limited to 1 MiB; active command presentation across
the workspace is limited to 8 MiB. Admission failure retains previous results.

## Durability and failure

Attempts are reserved durably before possible payload delivery. Their scope binds
the view, command stage, command definition and preceding native definitions; a
changing live-data revision does not grant another attempt. An explicit later run
reuses Ready records and delivers only never-attempted records. A reserved or
failed record with no usable result is not implicitly retried. Capacity is 100,000
attempts per scope, with no automatic eviction.

A globally invalid batch, including replies without a valid completion frame,
cannot become successful by pressing Run again: newly reserved records finalize
as Failed. A valid completed batch can preserve independent successful records
alongside event-level failures. Previously finalized results are immutable.

The saved command definition and the last published result reference are separate.
Editing a command must not destroy the old result set. Restoring a definition or
published result never launches the command. A crash after reservation but before
durable finalization leaves attempted records unavailable; cancellation does not
undo an attempt that may already have been delivered.

Workspace schema v4 stores the attempts and results. **Preview033 and older cannot
open a database migrated to v4.** No user database was migrated during implementation
tests. This compatibility change applies when opening a workspace in preview034.
