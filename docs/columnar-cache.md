# A derived columnar cache beside the journal

Design note, 2026-09-08. No code. It answers one question — would a column store
beside the journal be worth building — and says at what point it starts paying.

## Why lvu does not already store columns

Arrow is already here: Polars executes every filter over Arrow arrays in memory
(`docs/architecture.md`). What is not columnar is what lvu *keeps*. The journal
holds exact captured bytes, delimiters, invalid UTF-8, capture timestamps and
physical identities, appended a record at a time while a source is live, with no
schema. Those are the invariants — raw bytes, live append, no declared schema —
and a column store satisfies none of them well. So the journal stays as it is.

The gap is that every scan rebuilds columns from those bytes, every time. At
620k records a filter reads and decodes journal frames for 0.33 µs a record and
builds a Polars frame for another 0.19 µs, which is 55% of the 0.95 µs a record
the whole scan costs (`docs/performance.md`). A derived, disposable column store
would pay that once per segment instead of once per scan.

## What it would hold

Per sealed journal segment, one file of columns:

- `_lvu_source_id`, `_lvu_sequence` — identity, and the row order itself.
- `_lvu_captured_at_unix_nanos`, and the basis time when one is declared.
- `raw` as a **Binary** column, not Utf8: the record's bytes exactly as captured,
  including invalid UTF-8, which a Utf8 column cannot hold without changing them.
  This is the important one. With `raw` present a literal or regex search is an
  engine scan over one contiguous buffer, so the cache covers the filter people
  actually run rather than only typed predicates.
- The most-filtered top-level fields, typed — level, service and whatever else a
  view has pinned or filtered on — chosen by observed use, not declared.

Enrichment outputs stay out. They belong to an accepted chain at a revision, the
chain is edited far more often than a segment is sealed, and `EvaluationBatch`
provenance already replays them at their original batch boundaries. Caching them
would mean invalidating the segment on every stage edit, which is the case the
cache is worst at.

## Format

Arrow IPC per sealed segment, not Parquet. Both are fine to read; IPC is
mappable and needs no decompression pass, and nothing here wants Parquet's
footer statistics badly enough to pay for encoding them — the journal is the
authority for what exists, and the cache is read whole or not at all. Segments
are sealed at a size that keeps one of them a bounded read; the live tail has no
segment and is served from the journal as it is today.

It lives under the existing derived-index budget and reports through the same
`RowReadiness` vocabulary: a segment that has no cache yet is `Indexing`, one
that could not be built is a reason string, and neither blocks a scan.

## Invalidation

The cache is a projection, so anything that changes what it projects drops it:

- **Source generation.** A restart re-acquires; the cached segments for the old
  generation are dead, exactly as the V3 row index already treats generation.
- **Journal identity.** A different capture root or a rebuilt journal is a
  different file; segments bind the acquisition identity as the row index does.
- **Partial-line completion.** A record flushed before its terminator arrives is
  rewritten when the rest of the line lands. A segment is only sealed behind the
  completion watermark, so this cannot reach a sealed segment — and that is the
  rule that makes sealing safe, not an afterthought.
- **Schema drift** does not invalidate anything: the typed columns are a
  projection of some fields, and a record whose field is absent is null. A field
  that starts appearing later simply is not in older segments, and the scan
  falls back for those.

The enrichment chain revision does *not* invalidate it, because enrichment
outputs are not in it.

## How a scan uses it

Unchanged in shape: the view worker walks sources and pages. For a sealed
segment with a cache it reads the columns instead of the journal, applies the
same Polars predicate, and gets the same matched identities. For the live tail,
or a segment with no cache, it reads the journal exactly as now. One scan can mix
the two; the results are identities either way, so nothing downstream changes.

The predicate is the engine's, not ours. `str.contains` over the Binary column
is the operation, the same expression the current path builds — the point of the
cache is to hand Polars a column it did not have to construct, not to hand-write
a scanner.

**One correctness detail that is easy to miss.** An Arrow Binary column stores
values back to back with no separator, so a needle can match the buffer while
lying across two adjacent records. Every hit has to be checked against the
offsets. That is a binary search per hit, negligible when a filter is selective
and irrelevant when it is not, because a filter matching most rows is returning
most rows anyway. Any claim that this "scans one contiguous buffer" is only true
with that check in it.

## What it would gain

Measured, engine ops only, over one column of ordinary records
(`crates/lvu-query/tests/binary_column_scan.rs`, this host, dev profile):

| Operation | 620k rows / 72 MB | 3M rows / 352 MB |
| --- | ---: | ---: |
| `to_lowercase().contains_literal` — today's case-insensitive search | 414 MB/s | 364 MB/s |
| `contains_literal` — case-sensitive | 2,377 MB/s | 3,715 MB/s |
| `contains("(?i)…")` — case-insensitive as a regex | 1,516 MB/s | 1,504 MB/s |

Against today's whole scan at 590 MB/s of journal, or 0.95 µs a record.

Extrapolating a filter over a fully cached source, taking the `(?i)` figure and
adding today's membership and publication cost, which the cache does not change:

| Records | Journal | Text | Today | Cached | |
| --- | ---: | ---: | ---: | ---: | --- |
| 620k | 118 MB | 72 MB | 0.59 s | ~0.11 s | 5x |
| 3M | 570 MB | 350 MB | 2.9 s | ~0.5 s | 6x |
| 30M | 5.7 GB | 3.5 GB | 29 s | ~9 s | 3x, and I/O-bound |

At 30M the cache no longer fits in page cache, so the scan becomes a 3.5 GB read
from a volume that measures a few hundred MB/s — the compute win is real but the
disk takes it back. That is the honest shape: the cache is worth most in the
middle, where the data exceeds what a per-page rebuild handles comfortably and
still fits in memory.

**A cheaper result fell out of the same measurement.** Today's case-insensitive
search lowercases the whole column and then searches it, at 414 MB/s; expressing
the same search as a `(?i)` regex is 1,516 MB/s — 3.7x — and needs no cache, no
new file format and no invalidation rules. It is one expression change in
`TextSearch`. If any of this is worth doing first, it is that.

## What it would cost on disk

A journal frame is 16 header bytes plus 58 fixed body bytes over a record whose
payload averages about 109, so the journal is 1.68x its source. The cache's
`raw` column is the payload plus an 8-byte offset, about 1.07x the source —
*less than the journal* — and the typed columns add roughly 20 to 30 bytes a
record. Per gigabyte of journal, expect about **0.8 GB of cache**. Held on top of
the journal, not instead of it, so a cached source occupies about 2.5x its
source rather than 1.7x.

It is disposable and under the derived-index budget, so the honest framing is
that the budget has to be big enough to be useful: a cache that is evicted
before it is read twice has cost a build and returned nothing.

## Does it replace the row-offset index?

Partly, and the part matters. A `raw` column with offsets *is* a row index for
the segment it covers: row *i*'s bytes are at `offsets[i]`, which is what the V3
index exists to answer. So for sealed segments the index is redundant.

It is not redundant for the live tail, which is where the row index does its real
work — serving the viewport while capture is still running, before anything can
be sealed. So both exist, and the budget holds both. Replacing the index outright
would mean sealing segments while they are still being appended to, which trades
this cache's simplest invariant for a second copy of the hardest problem.

## Risks

- **A second copy of typed data disagreeing with the bytes.** The journal is the
  source of truth for identity, restart and the raw-bytes guarantee; the cache is
  a projection that must byte-compare equal to it. That needs a verification pass
  — sampled, not exhaustive, because exhaustive is just rebuilding it — run when
  a segment is sealed and on a bounded sample when it is read. A mismatch drops
  the segment's cache and falls back to the journal, and says so.
- **Complexity in `run_query`.** The scan loop is already the most intricate code
  in the view worker: checkpoints, generations, evaluation provenance, group
  continuation across pages. Adding a second source of records with its own
  readiness state is real risk in the function least able to absorb it.
- **Cache-shaped bugs are silent.** A stale or wrong cache returns *plausible*
  results. Every other failure in this product is visible; this one would not be.

## Recommendation

Not yet, and the threshold is not a record count.

The cache pays when a source is scanned repeatedly. Building it costs one pass;
each subsequent filter saves roughly four fifths of its scan. So it repays after
about **two filters over the same sealed data**, and a session that filters a
source once should never build one. That makes the trigger a usage signal — a
source that has been fully scanned more than once at its current generation —
rather than a size.

Given that, the order I would put these in:

1. The `(?i)` regex change. 3.7x on the dominant operation, one expression, no
   new concepts. Do this regardless of anything else here.
2. Re-measure. With the lowercasing copy gone, the scan's remaining floor moves
   to journal read and frame construction, and the cache's case gets *stronger*
   in relative terms while the absolute wait it removes gets smaller.
3. Only then consider the cache, and only if step 2 still shows people waiting.

The strongest argument for building it eventually is the 3 GB case, where a full
filter is tens of seconds and no amount of per-record tuning fixes that. The
strongest argument against building it now is that the cheap change above has not
been made, and a column store is a large amount of new invalidation surface to
add to a scan that has just been made six times faster by removing waste.
