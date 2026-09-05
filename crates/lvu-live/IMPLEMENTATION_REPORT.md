# Live journal-to-TUI paging adapter report

## Public seam

- `LiveRowProvider::new(LiveConfig)` creates bounded request/update queues and a
  formatted-row cache. Construction requires an existing Tokio runtime but does
  no journal I/O.
- `register_source(SourceHandle)` starts one background worker for the runtime
  generation. Same-generation registration is idempotent. A new generation
  joins the prior worker before taking the index lock, uses an independent worker
  epoch to fence stale updates, invalidates derived rows, and rebuilds the index.
  It never starts or duplicates acquisition.
- `register_raw_view(view_id, source_ids)` maps one or more shared sources into a
  raw view. Current multi-source order is deterministic source concatenation.
- The synchronous `RowProvider` methods only consult memory and use bounded
  `try_send`. Missing rows return a cached contiguous prefix while status reports
  loading/indexing; no method awaits or reads disk.
- `drain_ready_updates(maximum)` is the nonblocking terminal-tick bridge.
  `source_status`, `view_status`, and `stats` expose acquisition/index progress,
  physical-record counts, high watermark, errors, queue pressure, and cache use.
- `shutdown()` closes source/view/request admission, settles pending request
  state, and joins adapter workers without altering journals or stopping
  runtime-owned acquisition.

## Storage and identity

Each source has one disposable index under the configured artifact directory.
Its checksummed header binds source UUID, runtime generation, and journal paging
geometry so reopening with different page limits rebuilds before serving. Fixed-size,
checksummed entries map monotonic sequence and physical position to a bounded
journal page offset. The final entry commits each page; a valid but incomplete
crash suffix is rolled back to the preceding page boundary. Startup validates
every entry, and malformed, stale-generation, or misordered indexes rebuild from
the authoritative journal. A nonblocking exclusive lock prevents two adapters
from mutating the same artifact. No raw bytes are copied into the index.

All derived-index open, validation, append, flush, seek, and read operations are
serialized by one bounded blocking disk actor per source. Tokio runtime threads
only await its bounded command replies. Index growth and viewport service use
bounded one-request/one-page alternation.

Index storage is bounded per registered source. Reaching the configured ceiling
is exposed as `IndexState::Limited`; it stops derived growth but keeps serving
all previously indexed history and leaves the authoritative journal unchanged.

`maximum_total_index_bytes` adds a directory-wide ceiling and defaults to 5 GiB;
the settings owner can map `[cache.disk].total_mib = 5120` to it. A checksummed
ledger under the existing cross-provider ownership lock reserves every page
before mutation, preventing simultaneous writers from oversubscribing. The
ledger also persists the shared cap. A differently configured provider is
limited while any writer is active; once all writers close, bounded startup
reconciliation can adopt a new settings value. Failed writes/flushes attempt to
restore the original size and reconcile actual sizes before unlocking. If that
cannot be verified, the full reservation remains and growth stops. A bounded
startup reconciliation counts every direct `*.rows.idx` regular file, including
unknown/corrupt files conservatively. Truncated or unverifiable reconciliation
refuses growth with an explicit global-budget `Limited` explanation.

This implementation deliberately uses explicit admission rather than automatic
eviction on append. The accepted race-safe artifact helper remains the cleanup
path; successful removal of a verified unused recomputable index releases its
ledger bytes only after deletion. It cannot target journals, catalogs, cursors,
SQLite/TOML files, exports, or session data.

Cached rows retain `RowId(source UUID, sequence)`. Only requested records are
formatted. Invalid UTF-8 is decoded once and its lossy display projection is
truncated at a character boundary so each eligible row fits the cache budget in
linear time. Oversized projections are marked. Raw
index membership preserves physical fragment records, which are explicitly
labelled rather than claimed as complete logical lines.

## Validation scope

Tests use actual provisional `SourceManager` file and command acquisition. They
cover initial loading, partial command publication, invalid UTF-8, live tail
arrival, stable App selection, backwards paging over 600 physical records with a
six-row cache, two independent views sharing one source, a multi-source raw view,
generation replacement, fair preview during deliberately incomplete indexing,
same-generation idempotence, overlapping artifact ownership, committed-page
crash recovery, paging-geometry rebuild, index corruption/rebuild without journal mutation, continued
uncached paging after a derived-index byte limit, tiny-cache invalid UTF-8, and
cancellation with a saturated update queue, concurrent multi-source global
admission, continued capture/readback at the global limit, cleanup budget
release, restart reconciliation, and cross-provider reservation races.

This deliverable starts from integrated main `74f8336` and does not modify or
claim ownership of runtime/core, root manifests, settings persistence, the TUI,
or native query/Polars behavior.
