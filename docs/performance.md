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
server, including resize and restoration. SSH acceptance remains outstanding.
