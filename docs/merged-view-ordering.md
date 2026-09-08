# Interleaving a merged multi-source view by time — design

Status: design only. The implementation waits for W24's query-host round-trip
work to land, because the order would be built in the same publication path
W24 is changing. Companion to `docs/architecture.md` ("Merged views order
records by explicit source position, then physical sequence") and
`docs/component-model.md` §2.3.

## The defect

`Membership` holds `sources: Vec<SourceMatches>`, each with `sequences`
ascending, and display order is their concatenation: `membership_index` is a
prefix sum over the sources plus a binary search inside one. A view over an
API log and a worker log shows every API record, then every worker record,
whatever their times — which is exactly the shape a user opens a merged view
to avoid.

## What already exists

Nothing here needs a new read of the data. `SourceMatches` already carries

```rust
sequences: Arc<[u64]>,
/// The basis timestamp of each matched record, aligned with `sequences`.
times: Arc<[i64]>,
bounds: SourceTimeBounds,
```

and `Membership::basis` says which basis those times were read in. The merge
input is therefore k already-materialised runs, one per source, and the order
is a k-way merge over data the membership is already paying for.

## The order

The **display order** of a merged view is the k-way merge of the per-source
runs by the key

```
(merge_time, source_position, sequence)
```

where `source_position` is the source's index in the view's source list and
`merge_time` is defined by I3.

The order is materialised **once per publication**, as one `Arc<[(u32, u32)]>`
of (source index, offset) in merged order plus a per-source `Arc<[u32]>` of
ranks. Membership is already an immutable `Arc` snapshot, so the order is
built where the snapshot is, never during paging. `page` becomes a slice of
the order vector and `index_of_id` a rank lookup, so both keep the cost they
have today.

## Invariants

**I1 — Total and stable.** The key is a total order on `(source_id,
sequence)`: ties in time break by source position, then by sequence. No two
records compare equal, so the order does not depend on the merge's
implementation, and re-publishing an unchanged membership produces the
identical order.

**I2 — A source is never reordered against itself.** The merge consumes each
source's run in sequence order and does not sort inside it. If a source's own
event times are not ascending — an event-time basis over out-of-order arrivals
— the run is used as it stands.

  Arrival order is the one ordering lvu knows to be true. Sorting inside a
  source would claim an ordering the data does not support and would break the
  monotonicity `index_of_id` relies on. The consequence is stated rather than
  hidden: **a merged view is globally nondecreasing in time only when every
  source is; otherwise it is nondecreasing per source and merged between
  them.** The order row says which (below).

**I3 — A record with no basis time keeps its place in its own source.**
`NO_BASIS_TIME` never sorts to either end. A record with no readable value in
the basis takes the `merge_time` of the nearest preceding *timed* record in
its own source; a leading untimed run takes `i64::MIN` and so leads that
source's run.

  The basis is a *reading* of the data, not a property of it. Bunching
  unreadable records at one end would move records the user can see because of
  a parse, and dropping them is out of the question — All events shows every
  record. This is the same rule `find_membership_gap` already follows when it
  skips untimed records without inventing a distance for them.

**I4 — `index_of_id` is the merge position, and stays cheap.**
`membership_index` returns the count of records that sort before `wanted`,
read from the per-source rank vector: O(k + log n), the cost it has today.
Every caller — selection, scrolling, bookmark jumps, `jump_to_record`,
correlation origins — keeps working unchanged, because all of them index the
displayed stream and none of them assume how it was built.

**I5 — A fold run is contiguous in merged display order.** Folding groups
adjacent rows with an equal fold key, and "adjacent" means adjacent in the
merged order, not within a source. `FoldScope::Adjacent` is adjacency in the
merged order; `FoldScope::Lookback(n)` is a window of n merged positions.
A fold run is a contiguous range of merged positions, which is what lets
`fold_display_index` keep its contract: it maps an unfolded position to a
display position, and only the meaning of "unfolded position" changes.

  This is a capability, not just a port: under concatenation a run spanning
  two sources could never be adjacent, so interleaving is what makes folding
  a chatty service across two logs possible at all.

**I6 — A live append inserts mid-stream without moving the selection.** A
record can arrive whose time is older than the last displayed record; it takes
its merge position, which may be above the viewport. The selection is held by
`RowId` and re-resolved through `index_of_id` after each publication, so:

- the selected `RowId` still resolves to a record after the insert;
- the selected record stays on the same screen row while it is visible, the
  scroll offset absorbing the records inserted above it;
- follow mode continues to mean *the last record in display order* — the
  greatest merge key — not the most recently arrived. A late record with an
  old time therefore does not steal the viewport.

**I7 — Identities are stable under reordering.** `RowId(source_id, sequence)`
is unchanged and never renumbered. Nothing that outlives a frame refers to a
display index: bookmarks, the selection, correlation origins and fold
expansion memory are all keyed by identity. A bookmark taken before an insert
resolves to the same record after it, at a different index.

**I8 — Adding or removing a source changes the order, never an identity.**
Source-list edits already publish atomically with query membership
(`architecture.md`), so a page is never drawn against a half-built order.

## What the Time dialog's order row shows

W19's part 3 adds an order row that reads `capture (arrival)` today. For a
merged view it must name the basis the *order* uses — the membership's basis —
and, because of I2, whether the order is fully sorted:

| View | Row |
| --- | --- |
| Any view on capture time | `Order   Capture (arrival)` — capture time is assigned by lvu on ingest, so a merged capture order is genuinely sorted |
| Merged, every source ascending in the basis | `Order   Event time` |
| Merged, some source not ascending | `Order   Event time · 2 of 3 sources arrive out of order` |
| A source with unreadable values in the basis | the existing `event time: N missing, M invalid` readiness line already says so; the order row does not repeat it |

The "out of order" count is one `ascending: bool` per source, set by the same
single pass that builds the order, so the row costs nothing to render.

## Cost and bounds

| What | Bound |
| --- | --- |
| Order vector | 8 bytes per matched record, into the existing `MemoryBudget` alongside the membership it belongs to |
| Rank vectors | 4 bytes per matched record, same budget |
| Build | O(n log k) once per publication, k = source count |
| `page` | O(page), a slice of the order vector |
| `index_of_id` | O(k + log n), unchanged |

The membership cap already bounds n; nothing here introduces an unbounded
structure.

## Where this collides

The order is built where the membership snapshot is published, in
`crates/lvu-view/src/lib.rs`. That is the same path W24 is changing on
`perf/query-latency` (per-batch filtering waits), which is why this is design
and tests now and implementation after W24 lands. `find_membership_gap` and
`time_bounds` both walk `membership.sources` "in display order" and will need
to walk the order vector instead; both already have the comment saying they
assume concatenation, which is where to start.

## Decisions that are the user's

1. **Should a merged view default to event time rather than capture?**
   Recommendation: no. Capture is the only basis every record is guaranteed to
   have, and a merged capture order is fully sorted (I2 cannot bite), so it
   cannot fail. Event time stays a choice the Time dialog offers.
2. **Should a source with out-of-order event times be sorted inside itself?**
   Recommendation: no (I2). Say it in the order row instead. Sorting would
   make the view claim an ordering the source never promised, and would
   silently disagree with the same records shown in that source's own view.
3. **Should follow mean "greatest merge key" or "most recently arrived"?**
   Recommendation: greatest merge key, so FOLLOW and the display order agree.
   The alternative makes the viewport jump to a record that is not at the
   bottom of the screen.

## Tests

`crates/lvu-view/tests/merged_ordering.rs`. The ones that hold today are live
and pin behaviour across the change; the ones that describe interleaving are
`#[ignore]`d with the reason, so they compile against the API from the start
and are un-ignored by the implementation commit.

Live now:

- `index_of_id` agrees with the record's position in `page` for every record
  of a two-source view — the property every selection and jump rests on, and
  the one most likely to break silently when the order changes.
- `page` over a two-source view is a permutation of the records: no
  duplicates, no omissions, `total` equal to the count.
- A bookmark-style identity resolves to the same record after a live append
  (I7).

Ignored until the engine interleaves:

- The merged page is nondecreasing in the basis time when both sources are
  (I1, I2).
- Ties break by source position then sequence, and re-publishing gives the
  identical order (I1).
- A record with no basis time keeps its place after the preceding timed record
  of its own source (I3).
- A fold run spanning two sources is contiguous in display order, and a run
  that alternates by arrival but is contiguous in time folds at all (I5).
- A late append with an older time lands at its time position, `index_of_id`
  reports the new position, and the previously selected identity still
  resolves (I6, I7).
