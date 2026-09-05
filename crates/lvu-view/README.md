# lvu-view

`lvu-view` is the asynchronous native query adapter between durable live sources,
the existing `lvu-query` engine, and the terminal's `RowProvider` / `QueryDispatcher`
seams. Unconstrained views delegate immediately to `LiveRowProvider`. Constrained
views scan bounded journal pages and atomically publish immutable, per-source
stable-sequence snapshots only after the composite request validates and completes.

Literal search uses `lvu_query::TextSearch`; Python is never started for a literal-
only request. An advanced expression is compiled lazily by the configured
`CompilerHost`, then executed with literal search as an AND constraint by the native
engine. A failed or stale candidate leaves the previous applied membership intact.
Leading-slash named-capture enrichment is compiled natively and may yield several
ordered derived columns from one definition. Membership display and immutable
snapshot replay retain every output rather than collapsing the definition to one
column. The internal stage collection is also the execution boundary for ordered
multi-definition chains. The adapter consumes the typed vector only; persistence/UI
migration from the legacy single string is owned by their integration layers and
must not re-encode a chain inside one opaque source string.

The caller should register each `SourceHandle`, then register views, periodically
call `drain_updates`, keep the adapter as the mutable `QueryDispatcher`, and pass
the cloneable handle returned by `rows()` as the terminal's `RowProvider`.
Membership paging and identity lookup are bounded in-memory operations;
`RowProvider` delegates row formatting/cache misses to `LiveRowProvider`.
`ViewQueryStatus` reports pending scan progress/high-watermarks, diagnostics, and
explicit membership/cache limits. Row formatting remains owned by the raw live
provider; absent cached rows appear on a later tick.

Membership snapshots are `Arc`-owned and account their sequence storage against a
single configured total memory cap, including simultaneously referenced applied and
candidate snapshots. `maximum_index_bytes` retains its public compatibility name but
now configures this membership-memory cap; `artifact_dir` is retained in the public
configuration for app compatibility but this adapter writes nothing there. This slice
does not persist view definitions, merge event-time ordering, or preempt a Polars collect: cancellation
is checked between bounded journal batches. New journal records are incorporated by
coalesced bounded refresh work during `drain_updates` ticks. Accepted compiled
definitions and schema state are reused in-session. Per-source generation, journal
offset, and last-sequence checkpoints ensure refresh evaluates only appended records
through a fixed high-watermark. Checkpoints are intentionally not persisted yet.

The cap is an admission/accounting bound for immutable membership payloads, not an
RSS guarantee. Rust `Vec` growth and conversion to `Arc<[u64]>` can temporarily use
additional allocator capacity during worker construction; query-engine working memory
is likewise separate. Published snapshots themselves retain exact-length slices.

## Investigation snapshots

`NativeViewAdapter::start_snapshot` freezes the currently published applied revision,
membership, source generations, and source high-watermarks. It starts a bounded worker
which writes typed source and filtered/enriched Parquet parts beneath a newly owned
investigation directory. `SnapshotJob::poll` is nonblocking; cancellation is observed
between bounded journal pages and is not claimed to interrupt a Polars evaluation or
Parquet write already in progress.

Admission performs no filesystem work. Directory creation, validation, scans, and
writes all occur in the worker. Immutable membership retains every accepted evaluation
batch boundary and its pre-evaluation schema context. Enrichment is replayed against
those exact full batches before membership selection; smaller export pages are sliced
only after evaluation. Thus incremental refreshes, cross-source schema evolution,
filtered-out type conflicts, and export part sizing cannot change the applied typed
interpretation. This provenance is charged to the configured membership memory cap.

Only an atomically renamed `manifest.json` denotes completion. Cancelled, limited, or
failed jobs remove their partial part directories and never publish that marker. The
source parts contain every captured record through the frozen boundaries, so filtered
parts are a convenience view rather than the agent's only data access. Each part lists
its physical schema because tolerant projection can evolve between parts. Missing
values remain null, conflicts retain `_lvu_type_*` provenance, and nested JSON remains
a string representation pending the documented evolving nested-schema work.
The adapter currently receives applied constraints rather than the persisted named
`ViewDefinition`; the manifest therefore records the exact applied generation/revision,
literal/advanced source, enrichment source, and compatibility ID. A later persistence
owner must add named view/recipe IDs and ownership pins when those objects are wired.
An export also refuses completion if a source generation changes, its journal ends
before the frozen high-watermark, or the exported filtered cardinality differs from the
immutable applied membership.

Limits bound journal rows/raw payload bytes, page working sets, part count, output disk
bytes, and concurrent workers. Parquet codec working memory and allocator/RSS overhead
are not represented by the byte counters. Dropping a job requests cancellation without
blocking the UI thread; callers needing teardown confirmation may call `wait` away from
the UI thread after cancellation.
