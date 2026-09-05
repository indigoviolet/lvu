# lvu-command-enrich

Persistent JSON Lines command enrichment for trusted local executable tools. Each
input and output is correlated by stable `{source_id, sequence}` identity; output
order is irrelevant and never changes input order. JSON values remain typed,
including nulls, numbers, arrays, and objects. Derived fields are additive and
cannot target `raw` or `_lvu_*` names.

The managed protocol frames every batch explicitly. Host input is
`batch_begin(session, revision, event_count)`, typed `event` objects, then
`batch_end`; command output contains correlated `event` objects followed by one
matching `batch_complete`. Completion before all events, a missing completion,
duplicate/delayed output, or stale session/revision poisons and resets the stream.
Nonempty batch diagnostics mean the completion protocol failed globally. Fields
from independently successful events remain available, while every unfinished
event receives its own failure reason.

The runner bounds events, outstanding IDs, serialized input, response lines and
total output, retained stderr, diagnostic text, queues, and wall time. Timeout,
cancellation, EOF, or a poisoned protocol kills and waits for the owned Unix
process group and joins pipe workers before another batch. These controls are not
a security sandbox or an OS CPU/memory quota; commands are user-authorized code.

Successful command results may be nondeterministic and are not automatically
rerun. Higher layers must persist them as durable derived captures keyed by the
reported command revision. Changing revision requires a new runner/session.
Callers own a capacity-bounded `AttemptLedger`; IDs are committed only at the
conservative send boundary after bounded serialization and successful spawn, survive
process resets, and are never evicted or silently rerun. Full ledgers reject
new work explicitly. Higher layers must durably persist or transfer the complete
deduplication set when replacing a ledger; rotating to an empty ledger permits rerun.
