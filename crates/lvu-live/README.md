# lvu-live

`lvu-live` adapts durable `lvu_ingest::SourceHandle` journals to the synchronous,
bounded `lvu::RowProvider` seam. UI calls never await or perform disk I/O. They
read bounded memory state, enqueue page/identity requests with `try_send`, and
return the currently cached contiguous prefix plus the indexed physical-record
count. Background source workers fairly alternate index growth and viewport work.
A dedicated bounded blocking disk actor owns each disposable checksummed index;
Tokio runtime threads do not execute its filesystem operations.
The index header binds the page record/byte geometry used to create its offsets;
changing either limit causes a safe rebuild from the journal.

Sources are registered once and shared by any number of raw views. A raw view
with multiple sources currently concatenates sources in configured order; native
query membership and cross-source ordering belong to the query integration seam.
Fragments remain separately visible physical records and are labelled `fragment`.
Lossy/truncated text is display-only; authoritative bytes remain in the journal.
`IndexState::Limited` stops index growth at its disk budget while retaining
read access to already indexed history.

`LiveConfig::maximum_index_bytes_per_source` defaults to 256 MiB and
`LiveConfig::maximum_total_index_bytes` defaults to 5 GiB. Settings integration
should map `[cache.disk].total_mib = 5120` to the latter using checked MiB-to-byte
conversion. `StorageBudget` exposes both limits to callers.

The global total is coordinated across providers and processes sharing the same
artifact directory by the existing ownership lock plus a persistent accounting
ledger. The ledger persists one shared cap: while any index writer is active,
providers configured with a different cap are limited rather than weakening the
live cap. After all writers stop, the next bounded reconciliation may adopt a
new settings value. Startup reconciliation examines a bounded number of direct children;
unknown or corrupt `*.rows.idx` files are counted by size and never removed. If
the scan is truncated or metadata cannot be verified, growth stops explicitly
as `Limited` rather than claiming a strict filesystem quota.

Automatic eviction is intentionally not performed on the append path. The
reviewed `inspect_derived_artifact` and `remove_unused_derived_artifact` API is
the cleanup seam: it removes only locked, validated, unused recomputable indexes
and releases their budget after successful mutation. Raw journals, catalogs,
cursors, databases, settings, exports, and session data are outside that seam.
Failed rebuild or append mutations restore the verified original length when
possible and always reconcile observed sizes before the ownership lock is
released; if reconciliation itself fails, the reservation remains conservative
and further growth is refused.

## Integration sketch

```rust,ignore
let manager = SourceManager::new(capture_root, RuntimeConfig::default())?;
let handle = manager.start(definition).await?;

let rows = LiveRowProvider::new(LiveConfig::new(derived_cache_root))?;
rows.register_source(handle.clone())?;
rows.register_raw_view("raw", vec![handle.source_id()])?;

let mut app = App::new(source_items, view_items, false);
loop {
    rows.drain_ready_updates(64); // nonblocking terminal tick
    app.sync_provider(&rows, viewport_height);
    // render/event dispatch uses &rows as the RowProvider
}

rows.shutdown().await; // stops adapter workers only
manager.shutdown().await; // owns acquisition shutdown
```

`view_status`, `source_status`, and `stats` expose acquisition state, generation,
reported versus indexed records, indexing/loading state, physical-fragment
semantics, cache use, and bounded queue pressure for later UI presentation.
