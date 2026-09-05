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
configuration for app compatibility but this adapter writes nothing there. This slice does not persist view
definitions, merge event-time ordering, or preempt a Polars collect: cancellation
is checked between bounded journal batches. New journal records are incorporated by
coalesced bounded refresh work during `drain_updates` ticks. Accepted compiled
definitions and schema state are reused in-session. Per-source generation, journal
offset, and last-sequence checkpoints ensure refresh evaluates only appended records
through a fixed high-watermark. Checkpoints are intentionally not persisted yet.

The cap is an admission/accounting bound for immutable membership payloads, not an
RSS guarantee. Rust `Vec` growth and conversion to `Arc<[u64]>` can temporarily use
additional allocator capacity during worker construction; query-engine working memory
is likewise separate. Published snapshots themselves retain exact-length slices.
