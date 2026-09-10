# Platform validation evidence

Audit baseline: source `9fc7e5363d975c231b7f5d8fe1e16c3e249a7752`, 2026-09-09.
This is an evidence map, not a claim that every packaged platform is supported.
The published release evidence is for the immutable v0.1.3 source
`8ca75d0074aa85964f5d493d88d1be3a48fcc0f4`; current-source rows say so when
their dedicated hosted run is still pending.

## Evidence matrix

| Boundary | Linux | Native macOS | Native Windows |
| --- | --- | --- | --- |
| Workspace compile | Accepted by the v0.1.3 release gate; current source also has Linux acceptance in the work ledger. | Hosted run 34423899078 compiled every target on both native architectures. | **Blocked in current source.** The first compiler boundary is `lvu-core/src/journal.rs`: Unix fd imports and metadata identity methods are not target-gated. Unix-only subprocess code in `lvu-query` and `lvu-command-enrich` remains latent behind that dependency failure. |
| Packaged/install-tree startup | v0.1.3 archive and installed archive passed executable/resource/helper checks and PTY acceptance. | Both v0.1.3 archives executed `packaging/stage.sh` on native hosted runners, including resource lookup and a real helper compile. No actual Homebrew or mise install was run on a Mac. | No archive or install exists because the workspace does not compile. |
| File capture and terminal cleanup | Full Linux PTY evidence is recorded in `docs/work-ledger.md`. | Source uses the same `/dev/tty`, termios and Unix process-group paths. Current-source native kernel-PTY evidence is **pending** the dedicated workflow. This is not Terminal.app or iTerm2 evidence. | No evidence. Crossterm would use its separate Windows console backend; the Unix PTY harness is not equivalent to ConPTY. |
| Command process cleanup | Orderly stop/quit and Linux parent-death cleanup are tested. | Unix process-group cleanup exists for orderly stop/quit. Darwin has no Linux `PR_SET_PDEATHSIG` equivalent here, so `SIGKILL` can orphan the owned command tree. | Immediate-child kill exists behind Tokio, but process-group helpers are no-ops. Descendant cleanup is not implemented; Job Objects are required before command capture can be supported. |
| Piped stdin while keyboard remains interactive | Supported and covered by the Linux PTY suite. | Explicitly returns `Unsupported` for a FIFO because reopening `/proc/self/fd/0` is Linux-only. The dedicated harness treats this exact diagnostic as a known limitation. | Explicitly returns `stdin capture is not yet supported on this platform`; the app cannot currently compile far enough to exercise it. |
| Derived-index cleanup | Linux handle-relative enumeration and exchange/unlink are implemented and tested. | Enumeration and reviewed exchange/unlink return `Unsupported`; ordinary raw capture remains durable, but storage cleanup is unavailable. | Handle-relative open is unsupported, and non-Unix identity fallbacks are not sufficient for safe cleanup. |
| Terminal emulator capabilities | Linux PTYs cover emitted SGR mouse, paste, resize and restoration sequences. | No Terminal.app/iTerm2 hosted interaction exists. The kernel PTY can prove input and emitted restoration sequences only; mouse decoding, OSC 52 delivery and colour rendering remain human-host checks. | No ConPTY harness. No mouse/paste/resize/restoration acceptance may be inferred from macOS or Unix PTYs. |

Authoritative published evidence: GitHub Actions release run
<https://github.com/indigoviolet/lvu/actions/runs/34411091946> succeeded on
native `macos-15` arm64 and `macos-15-intel` runners and published both Darwin
archives. Its `Stage and verify the archive` step executes the staged binary,
checks direct and symlinked resource lookup, compiles a Polars expression with
the packaged helper, and loads the packaged bridge dependencies. It does not
open a graphical terminal emulator or install through Homebrew/mise.

Current-source hosted evidence: pull-request run
<https://github.com/indigoviolet/lvu/actions/runs/34423899078> tested PR head
`b1358d02909cadcd06c41704662fcaca59e2ea13` through merge source
`0bdcd034afd5ba7ecb375becbb0d4069bc686bd5`. Both native Darwin all-target
compiles passed. Intel therefore provides compile evidence only. Arm64 stopped
in `packaging/stage.sh` before runtime because macOS Bash rejected an empty
array expansion under `set -u`; no arm64 install-tree or runtime result may be
claimed from that run. Windows stopped first in `lvu-core/src/journal.rs`; its
artifact records seven exact Unix-fd/metadata diagnostics and is blocker
evidence, not Windows support.

## Dedicated hosted workflow

`.github/workflows/platform-validation.yml` runs on an explicit dispatch or a
pull request that changes platform-relevant paths. It performs three distinct
checks without weakening one into another:

1. Both native Darwin runners run an all-target workspace compile probe.
2. The arm64 runner builds a complete development-profile install tree and
   drives the staged binary through a real kernel PTY. It covers help/resource
   resolution, file capture, Ctrl-C restoration, orderly shell-and-child cleanup,
   and the exact current piped-stdin refusal.
3. A Windows Server 2022 runner compiles each affected target crate separately
   and accepts only the exact currently first dependency diagnostics from
   Cargo's JSON stream, including their package, source path and multiplicity.
   Mixed or additional compiler failures are rejected. A newly successful
   compile also fails the characterization so it must be replaced by a real
   success expectation. This green job is evidence of a blocker, never evidence
   of Windows support.

Each job uploads JSON plus the complete compile log under a run-, attempt-, and
target-specific artifact. The JSON binds the source SHA, host, Rust version,
target, result and hash of the exact uploaded log bytes. The runtime JSON is
written on success or failure, binds the exact binary hash, retains bounded PTY
transcripts, and lists the terminal capabilities it did not test. An injected
failure immediately after the command child starts proves that the harness
records and stops only its owned process group before emitting failure evidence.

Run the same probes locally through the pinned environment:

```sh
export CARGO_TARGET_DIR=/mnt/HC_Volume_106796581/lvu-build/lvu-sol-platform-validation-target
evidence=/mnt/HC_Volume_106796581/lvu-build/sol-platform-validation-$(date -u +%Y%m%dT%H%M%SZ)
mkdir -p "$evidence"

flock /mnt/HC_Volume_106796581/lvu-build/sol-validation.lock \
  mise exec -- python scripts/test_platform_compile_probe.py

flock /mnt/HC_Volume_106796581/lvu-build/sol-validation.lock \
  mise exec -- python scripts/platform_compile_probe.py \
    --target "$(mise exec -- rustc -vV | sed -n 's/^host: //p')" \
    --expect success \
    --log "$evidence/compile.log" \
    --evidence "$evidence/compile.json"

flock /mnt/HC_Volume_106796581/lvu-build/sol-validation.lock \
  mise exec -- cargo build -p lvu-app --locked \
    --target-dir "$CARGO_TARGET_DIR"

flock /mnt/HC_Volume_106796581/lvu-build/sol-validation.lock \
  mise exec -- python tests/platform/test_unix_runtime.py \
    "$CARGO_TARGET_DIR/debug/lvu-app" \
    --expect-piped-stdin supported \
    --evidence "$evidence/runtime.json"

flock /mnt/HC_Volume_106796581/lvu-build/sol-validation.lock \
  mise exec -- python tests/platform/test_runtime_failure_cleanup.py \
    tests/platform/test_unix_runtime.py \
    "$CARGO_TARGET_DIR/debug/lvu-app" \
    --expect-piped-stdin supported \
    --evidence "$evidence/runtime-injected-failure.json"
```

On macOS, replace `flock ...` with another exclusive lock mechanism used by the
host and pass `--expect-piped-stdin unsupported`. When pointing the runtime
harness at a staged archive, add `--require-installed-resources`.

## First implementation blockers

The smallest honest next changes are separate product assignments, not hidden
inside this validation patch:

1. Gate the handle-relative journal implementation in `lvu-core` and fail
   closed on Windows until real handle identity/enumeration exists. Replacing
   `dev`/`ino` with length or timestamps would violate stable cleanup identity.
2. Gate and implement subprocess ownership in `lvu-query` and
   `lvu-command-enrich`; merely removing Unix imports is insufficient.
3. Add Windows Job Object ownership before enabling command capture or Docker
   discovery there.
4. Replace zero/length-only non-Unix derived-artifact identities with real
   handle identity or fail closed, then validate `LockFileEx` behavior on a
   native host.
5. Give macOS safe handle-relative derived enumeration/removal and a piped-stdin
   reader that does not leak `O_NONBLOCK` to the producer.
6. Build a separate Windows ConPTY harness. Do not port the Unix PTY test and
   call it equivalent.

Human macOS acceptance in Terminal.app and iTerm2 remains required for mouse,
Meta chords, resize/reflow, OSC 52 request handling, colour readability, and
post-exit terminal state. Signing/notarization also remains absent; this harness
does not change release policy.
