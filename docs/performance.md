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
