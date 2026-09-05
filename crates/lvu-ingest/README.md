# lvu-ingest

`SourceManager` owns at most one live acquisition and journal writer for each
`SourceId`. Cloneable `SourceHandle`s are view-independent: they share capture,
status, high-watermarks, and bounded page access without cloning history.

Records become visible only after their journal append and flush complete.
`synced_records` identifies the latest count covered by the configured fsync
cadence (`sync_every_batches`); graceful stop always drains already-read capture
events and fsyncs before reporting success. `journal_bytes` is allocated journal
size and does not itself imply that every record has crossed an fsync boundary.

`stop` is graceful and deadline-bounded. A false `StopReport::complete` means the
deadline expired; `discarded_bytes_known` is false because acquisition may still
hold bytes while background abort/cleanup releases the writer lock. `abort` is
immediate and may discard unjournaled bytes; `discarded_bytes` is a measured lower
bound covering observable queued and framer bytes, never a false exact claim. A
false `AbortReport::complete` likewise means caller-visible cleanup exceeded the
deadline; the runtime retains its lease until the non-preemptible disk operation
and owned writer have actually stopped. UI follow/pause is intentionally not a
source lifecycle operation.

Acquisition, writer, and page queues are bounded. A semaphore reserves a writer
control slot, so stop cannot be starved by record or page traffic. Only one page
request per source is outstanding, and page record/byte bounds are clamped by the
runtime configuration. Storage-limit failures stop acquisition and never evict or
overwrite raw capture.

Start reservations are owned independently of the calling future. Cancelling a
start cannot leak an `AlreadyRunning` reservation, and shutdown closes admission
before waiting (up to its configured deadline) for pending starts. Existing
metadata is read with a 1 MiB bound and its schema, identity, and complete source
definition must match before generation advancement. Catalog recovery validates
bounded complete JSONL entries, removes only a torn final entry, and durably marks
that previous run incomplete before appending new lifecycle events.
Changing any persisted definition field therefore requires a future explicit
replace-source operation; `start` intentionally does not perform replacement.

File and command acquisition are implemented. HTTP and command restart execution
remain explicit unsupported errors. On Unix, command shutdown owns the spawned
process group. The journal fsync cadence and stop deadline are configurable; no
fixed wall-clock durability guarantee is claimed beneath the filesystem.
