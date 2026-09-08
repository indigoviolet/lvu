# Local capture/query baseline

Run `CARGO_TARGET_DIR=/tmp/lvu-discovery-ui-target mise run bench:live`.
The ignored integration test uses synthetic records, real capture/journals and
native queries; it is excluded from ordinary unit-test runs. It checks results
and configured bounds without machine-dependent speed assertions.

2026-09-05 baseline on this 8-vCPU AMD EPYC-Rome Linux host, pinned Rust 1.98.1,
normal development profile, two build jobs, debug information disabled:

| Operation | Measured wall time |
| --- | ---: |
| Capture 50,000 records / 2,288,890 input bytes | 548 ms |
| Initial literal scan, 5,000 matches | 619 ms |
| First advanced compile and scan during appends | 1,383 ms |
| Above plus catch-up to 60,000 captured records | 1,507 ms |
| Three full time-bound revisions over 60,000 records | 3,635 ms |
| Warm incremental 1,000-record append | 86 ms |
| Six distant eight-row viewports, including return to evicted history | 305 ms |

Row cache: 32 rows / 128 KiB. Query membership admission: 128 KiB. Derived-index
limit: 8 MiB. Final reported membership: 48,928 bytes; row cache: 5,845 bytes /
32 rows; zero pending viewport requests. All expected stable IDs and 6,100 final
matches were verified. These counters do not represent total process RSS.

This is a short local baseline, not a sustained throughput or SSH guarantee.
Repeated full time-window scans remain materially more expensive than incremental
appends. The scheduler coalesces rolling ticks while a query is pending.

Actual context and bookmark PTY workflows also passed inside an isolated tmux
server, including resize and restoration. Authenticated loopback OpenSSH context
and bookmark PTYs also pass on preview031. Reproduce with `mise run test:pty:ssh`
when OpenSSH server/client tools are installed. The test uses an isolated daemon,
ephemeral port and temporary keys; it does not change user SSH configuration.
Remote network latency and disconnect behavior remain separate acceptance work.

## Bounded journal read-ahead

A follow-up uses a 64 KiB buffer per journal page. The same workload passed with
552 ms capture, 774 ms literal scan, 1,262 ms advanced compile/scan, 1,373 ms
catch-up, 3,779 ms for three time revisions, 69 ms warm append and 300 ms viewport
paging. These single-run wall times are similar to the baseline; they do not
establish an end-to-end speedup.

Separate `strace -f -c -e trace=read,pread64,lseek` runs of the same ignored test
measured 734,238 `read` calls before buffering and 2,715 afterward. Tracing adds
substantial overhead, so its elapsed times are not mixed with the table above.
The change reduces system-call overhead with fixed additional page memory; it
does not change journal format, checksums, page offsets, or query semantics.
Regression coverage includes page boundaries, a record larger than the buffer,
invalid UTF-8, byte limits and appending after a reader reaches EOF.

## Two-minute sustained capture/query run

`CARGO_TARGET_DIR=/tmp/lvu-discovery-ui-target mise run bench:live:sustained`
uses synthetic JSON with a paced offer of 400 records each 100 ms (4,000/s),
1,000 initial records and 1% matching rows. Three full fixed-time revisions run
during capture; viewport checks alternate between old history and the live tail.
The compiler is warmed before the measured interval.

2026-09-06 result on the same host:

- 481,000 records, complete in 119,992 ms; final capture and membership caught up.
- Maximum observed capture lag: 400 records (one offered burst).
- Maximum observed query lag: 328 matching records during full historical scans.
- 5,587 nonempty viewport samples verified exact stable record identities.
- Row cache stayed within 32 rows / 128 KiB; membership stayed within 128 KiB.
- Derived-index cap: 32 MiB. Final expected matches: 4,810.
- Linux reported Rust test-process peak RSS of 82,832 kB; this excludes the Python
  helper process and does not turn managed cache budgets into an RSS guarantee.

This establishes behavior for this offered rate, selectivity and two-minute
duration, not arbitrary log volume or indefinite operation. Full time revisions
can temporarily lag while ordinary incremental processing catches up afterward.

## Historical filter latency (2026-09-08)

A soak run reported a filter over 620k captured records taking 10.75 s (p50),
linear in record count and unchanged by selectivity, and read that as a wait:
"0.18 s of CPU across 10.57 s". Measuring the app's own `/proc/<pid>/stat`
across a single filter on the same 620k capture contradicts that. The filter was
compute-bound the whole time:

| 620k-record literal filter, real binary | wall | process CPU | CPU/wall | records per CPU-second |
| --- | ---: | ---: | ---: | ---: |
| before | 11.12–11.76 s | 15.54–21.60 s | 1.40–1.84 | 28,700–39,900 |
| after | 0.71–0.81 s | 0.74–0.88 s | 1.04–1.11 | 704,900–838,200 |

`mise run soak` over the same 620k source agrees. Before: p50 10.75 s, p99
11.05 s. After, over 24 timed filters on a box at load average 5 to 8:
**p50 0.727 s, p99 0.824 s**, at 1.10 seconds of app CPU per second of query
wall clock — busy, not waiting. That clears the soak's own one-second budget,
which it no longer reports as a failure. Resident memory across three cycles
went 27.1 to 53.3 MiB with a 61.6 MiB peak and no climb, capture held at
108.1 MiB, the derived index at 23.7 MiB, worst shutdown 0.135 s and slowest
input-loop iteration 0.037 s.

The p99 is the maximum of 24 samples, so it is the measurement most exposed to
whatever else the machine is doing, and it says so: the same code on a busy box
reported 1.629 s and 2.97 s for that maximum while its p50 barely moved
(0.754 s and 0.651 s). It is a tail, not a slow filter — driving all six of the
soak's literals by hand against the same capture, at load average 26 to 30,
gives

| filter | matches | wall | CPU | records per CPU-second |
| --- | ---: | ---: | ---: | ---: |
| `SOAK_MARKER` | 7 | 0.79 s | 0.55 s | 1,129,000 |
| `ERROR` | ~103k | 0.56 s | 0.52 s | 1,194,000 |
| `req-00042` | ~124 | 0.53 s | 0.48 s | 1,294,000 |
| `region` | ~73k | 0.76 s | 0.55 s | 1,129,000 |
| `malformed` | ~620 | 0.65 s | 0.55 s | 1,129,000 |
| `seq` | ~619k | 0.69 s | 0.61 s | 1,018,000 |

so no filter costs anything like three seconds even then, including the one that
matches nearly every record. A soak on
the same code produced no timings at all because the shared build volume had
filled to 1.1 GB free: capture stalled at 14,848 records and the run timed out
waiting for first rows. A soak measures the disk it runs on before it measures
anything else.

Nothing in the query path sleeps per record or per batch. The scan spent its time
doing work that did not need doing, in code compiled with no optimisation at all.
Both measurements are on this 16-core host with other agents building on it; the
"before" row was taken at load average 2.6 and the "after" row at 13.0, so the
improvement is if anything understated. `mise run soak` now records CPU per query
beside the wall time, so this cannot be misread the same way again.

Per-phase, for one literal filter over a settled file source, normalised to
microseconds per scanned record (`crates/lvu-view`, 200k records at the previous
256-record page size, and 620k records after):

| Phase | before | after |
| --- | ---: | ---: |
| Journal page read and frame decode | 3.42 µs | 0.33 µs |
| Records to a Polars frame | 1.75 µs | 0.19 µs |
| Identity check, predicate, matched identities | 12.0 µs | 0.36 µs |
| Membership, basis times, display grouping | 0.83 µs | 0.06 µs |
| **Total** | **18.2 µs** (55k records/s) | **0.95 µs** (1.05M records/s) |

What changed, in order of what it was worth:

- The `dev` profile compiled every dependency unoptimised, and lvu has no release
  build: previews, the PTY matrix and the soak all run the `dev` binary. An
  unoptimised Polars, `regex`, `memchr` and `crc32fast` ran under every scanned
  record. Dependencies now build at `opt-level = 2` and workspace crates at
  `opt-level = 1`; debug info and incremental compilation stay off. Rebuilding
  `lvu`, `lvu-view` and `lvu-app` after an edit takes about 38 s here.
- Query pages were 256 records, so the fixed cost of a journal round trip, a
  frame, a plan and an engine run was most of the scan. Pages are 4096 records,
  still bounded by the same 2 MiB byte limit.
- Every scanned row formatted its source UUID into a fresh 36-byte `String` for
  the identity column, and the per-batch identity backstop built a second
  `String` per row and inserted it into a `BTreeSet`. The identity column is now
  interned per page, the backstop compares in place, and owned identities are
  built only for rows that matched.
- Every scanned record was hashed into a `HashMap` of basis timestamps and a
  `HashSet` of matched sequences. Both are positional walks now.
- Journal frame decoding allocated and zeroed a fresh buffer per record, and
  open-time recovery read every frame with two unbuffered reads per record.

What bounds it now: the scan is O(bytes) with no fixed per-batch cost worth
naming. At 620k records over a 118 MB journal it reads and decodes at roughly
590 MB/s of journal, and the three remaining phases are within a factor of two of
each other, so no single one is the thing to attack next. A 3 GB source
extrapolates to about 15 s per full filter rather than about eight minutes.
Incremental refresh of an applied view still copies the whole matched-sequence
list per refresh; that is the next cost to look at, and it is a capture-time
cost, not a filter one.

Regression coverage: `crates/lvu-view/tests/scan_throughput.rs` scans 150k
captured records and asserts CPU-to-wall ratio and records per CPU-second, not a
wall-clock budget, so it fails for a scan that waits per batch on any machine
rather than for a slow one.

Capture was measured alongside so that the larger query pages could not hide a
regression in it. The same in-process capture of 620k records over 73 MB, with
the live provider and view adapter registered, went 7.0 MiB/s before, 8.1 MiB/s
with dependencies optimised and 9.8 MiB/s with workspace crates optimised as
well. It did not get slower. What it is bounded by is `fsync`, not this change:
during a soak, two of the app's tokio workers sit in `jbd2_log_wait_commit` at
about 5% of one core, and on this volume under other agents' builds `fsync`
measured a 19.8 ms median with a 5.4 s worst case, which took the same binary on
the same input from about 390 KB/s of journal to about 0.9 KB/s. That is its own
TODO row.

## Incremental refresh cost (2026-09-08)

An applied view refreshes when its sources grow. Each refresh publishes a new
immutable membership, and it built that membership by copying the previous one:
`sequences`, `times` and `groups` were each `to_vec`'d in and rebuilt into a
fresh `Arc` out, four passes over everything the view already held, to add the
handful of records that had just arrived. The cost followed the view, not the
arrival.

The symptom is not a slower second. Only one refresh is in flight per view, so a
refresh that grows more expensive simply happens less often — per-second CPU
flattens out while the per-refresh cost keeps climbing, which is why the first
measurement of this looked mild.

Measured per refresh, in process, over a view whose filter matches every record,
appending 20-record bursts (`crates/lvu-view/tests/refresh_cost.rs`):

| Membership | 20,000 rows | 160,000 rows | growth for 8x the view |
| --- | ---: | ---: | ---: |
| Rebuilt per refresh | 2.47 ms | 8.21 ms | 3.32x |
| Extended per refresh | 2.09 ms | 2.81 ms | 1.35x |

Isolating the part that scales: 41 ns per thousand rows held before, 5.1 ns
after — about eight times less, or roughly 25 ms against 3 ms of view-dependent
cost per refresh at 620k records. Repeat runs of the same test give 0.88x, 0.98x
and 1.08x for eight times the view, which is flat within the noise of a shared
machine.

End to end, the real binary with a chatty source appending 20 records a second
into a settled view, CPU per second of wall clock with the idle cost of the
same view subtracted:

| View | before | after |
| --- | ---: | ---: |
| 100k matched | 0.129 | 0.071 |
| 300k matched | 0.137 | 0.070 |
| 620k matched | 0.207 | 0.071 |

Flat where it grew, and 2.9x lower at 620k. The after column was measured at
load average 33 to 46 against the before column's 2 to 7, so the absolute
figures favour the before column if anything; what carries the result is that
the view-size term is gone.

What changed: membership is published as a list of immutable chunks
(`crates/lvu-view/src/appended.rs`) instead of one array. A refresh adds a chunk
and copies pointers to the rest, and chunks merge so their sizes stay strictly
decreasing, which bounds the count at `log2(len)` and costs each element
`O(log n)` copies over its whole life rather than one per refresh. Readers index
and binary-search across the chunks. Display grouping is the one caller that has
to reopen what it published — a run of repeated records can begin before a
refresh and continue after it — so the builder can take the last value back out
of the published chunks, which rebuilds only the smallest chunk.

What bounds it now: the fixed per-refresh cost, around 2 ms in that test, which
is the page read, the batch execution and the publication itself rather than
anything proportional to the view. `NativeViewAdapter::refresh_stats` reports
refreshes and the nanoseconds spent in them so this stays measurable.

## Reading the soak's numbers against capture (2026-09-08)

Every soak figure rests on a capture that is `fsync`-bound, and that is the
first thing to know before comparing two runs of it.

Capture measures about **1.55 MB/s of journal**, roughly 0.9 MB/s of source, on
this box when it is otherwise quiet. It is not throughput-bound: two of the
app's tokio workers sit in `jbd2_log_wait_commit` at about 5% of one core, so
the rate follows how long `fsync` takes, and `fsync` on this shared volume has
measured a 19.8 ms median with a 5.4 s worst case. The same binary on the same
input has run at 390 KB/s and at 0.9 KB/s as other agents' builds came and went
— more than two orders of magnitude, with no code involved. W25 owns that.

Three consequences for anyone reading a soak result:

- **The wall-clock figures are about the disk first.** A cycle time that moved
  between two runs says more about what else was writing than about the change
  under test. The CPU-normalised figures — CPU per query, CPU per refresh, the
  ratio between two view sizes — are the ones that carry a conclusion.
- **The p99 is the maximum of 24 samples**, so it tracks contention closely. The
  same code has reported 0.824 s, 1.397 s, 1.629 s and 2.97 s for it while the
  p50 stayed between 0.651 s and 0.860 s.
- **`soak:long`'s size is chosen against this rate.** Its settle wait is derived
  from the generated bytes at half the measured capture rate, and the default is
  800 MiB because 3 GB needs about an hour on this box — longer than any run is
  worth waiting for and more of a shared volume than it is fair to take.
  `--bytes` asks for more when there is time for it.

## Whole-view field statistics (2026-09-08)

The Fields dialog describes the selected field over the first 2,048 records of
the view (`docs/dialog-system.md` §8.12). With a scan running at about a million
records a second, the same figures over the whole membership are affordable, so
the pane now shows both: the sample instantly, and the whole view when it
arrives.

Measured, one pass over a settled view of integers
(`crates/lvu-view/tests/field_stats.rs`, ignored by default):

| Records | Bytes | Elapsed | Rate |
| --- | ---: | ---: | ---: |
| 620,000 | 27 MB | 3.14 s | 197,664 records/s |
| 3,000,000 | 133 MB | 13.72 s | 218,690 records/s |

Linear, and over the second the design allows for. What makes that tolerable is
the design rather than the number: the sampled figures render immediately and
stay on screen for the whole pass, the pass is superseded the moment the
selection moves, and a failure leaves the sample showing. Nothing waits for it.

That was the first measurement. Projecting only the column asked for, and
aggregating over larger buffers, brought it to 0.90 s and 4.06 s — see below.

Two divisions of labour are load-bearing, and both are commented where they
bite. The engine counts and the app names: the app decides what a field is from
the record's own bytes, hands that verdict over as a `StatsType`, and the engine
counts rows satisfying it rather than classifying anything — which is what stops
the sampled and whole-view figures disagreeing about what a number is. It is
enforced by the crate graph, because `lvu` has no Polars dependency and could
not hand over an expression if it wanted to. And the ordering that `min`/`max`
mean is the one thing the engine cannot express for itself: Polars compares
strings lexically, so a number spelled as text would sort "1000" before "9", and
the caller's cast is what makes the comparison mean what the app means.

One visible consequence: the sample stops counting distinct values at 4,096 and
reports a floor, while `n_unique` over the membership is exact. The same field
can therefore go from "4,096+ values" to "7 values", so the pane's heading
changes with the figures — `first 2,048 records`, `first 2,048 records ·
counting the rest`, `all 619,272 records` — rather than the numbers changing
under an unchanged caption.

## What a statistics pass was paying for (2026-09-08)

A whole-view pass over one field took 2.95 s at 620,000 records, about five
times a filter scan over the same data. Two costs, found by timing the pass's
phases rather than by guessing — the first two guesses were both wrong:

| Phase at 620k | Before | After |
| --- | ---: | ---: |
| Journal read | 0.21 s | 0.21 s |
| Projection | 1.46 s | 0.57 s |
| Aggregation | 1.15 s | 0.09 s |
| **Total** | **2.95 s** | **0.90 s** |

| View | Before | After | |
| --- | ---: | ---: | --- |
| 620k records / 27 MB | 2.947 s | **0.901 s** | 688,000 records/s |
| 3M records / 133 MB | 13.732 s | **4.058 s** | 739,000 records/s |

**Projection.** The pass called the canonical projection, which builds a column
for every field a record carries, ten of metadata and three copies of the
record's text — to read one of them. `records_to_field_column` parses the same
way, so JSON and logfmt records are read exactly as a filter reads them, and
materialises the one column asked for. It also converts only the wanted key: the
canonical parse turns every nested object back into text, work this pass then
discards.

**Aggregation.** Each batch ran several Polars plans, and the pass aggregated
once per journal page — 151 times over 620k records. Pages stay whatever size
the journal wants; how much is aggregated at once is now the pass's own
business, at 65,536 records. That alone took aggregation from 1.15 s to 0.09 s.

Two things that were *not* the answer, both measured before being discarded.
Letting Polars read the field out of the raw text with `json_path_match` is no
faster than parsing it here — 2.75 against 3.53 µs per record on ten-field
records, and *slower* on two-field ones (0.95 against 0.70) — and it would
silently undercount logfmt and unstructured records, which the sampled figures
read correctly. And the parse is not removable: it is 2.17 µs of the 2.68 the
targeted projection costs on ten-field records, so reading a field out of JSON
costs what it costs, whoever does it.

Guarded by `crates/lvu-view/tests/stats_throughput.rs`, which asserts records
per CPU-second rather than a duration. Measured both ways on that test: 320,000
records per CPU-second with the targeted projection, 113,000 with the canonical
one. The floor is 200,000, between them with roughly equal headroom, so a
return to projecting every column fails outright rather than merely being
slower.
