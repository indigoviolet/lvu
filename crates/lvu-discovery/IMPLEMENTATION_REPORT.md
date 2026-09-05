# M3 discovery foundation implementation report

## Delivered APIs and behavior

- `discover(DiscoveryRequest) -> DiscoveryResult`: asynchronous bounded provider
  coordination, deterministic deduplication, evidence merge, cancellation and a
  shared wall-time deadline.
- `DiscoveryCandidate`: reviewed `lvu_core::SourceDefinition`, label, provider,
  evidence, confidence, identity hints, availability, deterministic UUID-v5
  fingerprint, and byte-preserving canonical deduplication key. Persisted recent
  definitions retain their authoritative `SourceId` and configuration.
- `ProcConfig`: Linux `/proc` discovery for relative and absolute `tee` operands,
  regular-file stdout/stderr, and access-mode-confirmed writable regular fds.
  PID/parent/fd/access flags are transient evidence, not stable identity. Pipes,
  sockets and anonymous descriptors are rejected without opening them.
- `DockerConfig` / `DockerRunner`: structured JSON-lines `docker ps --all` input,
  context/labels/status evidence, logical Compose project/service/replica hints,
  and explicit per-container `docker ... logs --follow --timestamps --tail N ID`
  argument vectors. CLI stdout/stderr, drain time, cancellation, and process groups
  remain bounded and are reaped even after the direct CLI process exits.
- `ProjectConfig`: breadth/depth/file-bounded traversal of likely recent project
  log files without following symlinks, plus supplied recent definitions.

## Validation performed

All commands used the repository-pinned mise environment:

```text
mise exec -- cargo fmt --manifest-path crates/lvu-discovery/Cargo.toml --all
mise exec -- cargo test --manifest-path crates/lvu-discovery/Cargo.toml
mise exec -- cargo clippy --manifest-path crates/lvu-discovery/Cargo.toml --all-targets -- -D warnings
CARGO_TARGET_DIR=/home/venky/.paseo/worktrees/2hywlzbe/lvu-discovery/crates/lvu-discovery/target mise exec -- cargo test --manifest-path /tmp/lvu-review-discovery/Cargo.toml --offline
```

The final 11-test owned suite covers fake `/proc` relative `tee`, redirected/writable-fd
evidence, false pipe/read-only candidates, vanished process files and dedup; an
actual owned producer-to-tee process found via the real `/proc` and then captured
losslessly through `lvu-core`; and a controlled pipe whose ordinary reader still
receives every byte after discovery. Docker recorded fixtures cover malformed
JSON, Compose replica dedup, exact structured arguments, stable identity across
container-ID changes, project separation, oversized stdout, deadline cleanup,
and cancellation after direct-child exit. Project tests cover persisted-definition
precedence, recent-source dedup, tiny traversal/candidate bounds, byte-distinct
invalid-UTF8 filenames, filename selection, and symlink avoidance. The primary's
independent regression crate is also run as an explicit gate.

The optional live Docker container test was not run: the Docker executable exists,
but this environment cannot connect to `/var/run/docker.sock` (`permission denied`).
Recorded Docker fixtures run unconditionally.

## Integration notes and remaining scope

- The standalone crate manifest/lockfile must be reconciled by the primary into
  the root workspace; root `Cargo.toml`, `Cargo.lock`, and `mise.toml` were not
  modified.
- UI consumption/search/ranking and ordinary source-dialog integration remain
  owned by the UI/integration work. Discovery performs no mutations and starts no
  capture itself.
- Linux `/proc` is explicitly reported unsupported on other platforms. Docker
  process-group cleanup currently targets Unix, matching the initial Linux scope.
- Deleted-but-open descriptor targets are skipped because they cannot form a usable
  path-based source definition. Missing recent sources report unavailable and
  permission-indeterminate sources report unknown availability.
- No live Docker gate is claimed. No arbitrary user log payload was read during
  tests or discovery.

## Parallel fixture lifecycle follow-up

After integration, a broader parallel workspace run exposed intermittent Linux
`ETXTBSY` while executing the generated Docker shell fixtures. Running the three
Docker tests concurrently reproduced it immediately. The cause was test-process
forking during another test's short executable-fixture write window: the child can
temporarily inherit the writable descriptor until `exec` closes it. The Docker
fixture tests now hold one async mutex across each fixture's complete
create/use/drop lifecycle. This deliberately fixes the harness instead of adding
an unjustified retry to production Docker execution.

Validation for the follow-up ran the Docker test selection 50 times with eight
test threads (all passed), followed by the complete owned test suite, rustfmt,
clippy with warnings denied, and `git diff --check`.
