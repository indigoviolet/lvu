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
