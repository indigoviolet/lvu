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
