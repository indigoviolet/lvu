# Installation and distribution

Status: 2026-09-09. **`v0.1.1` is published; `v0.1.2` is being prepared.** The tag is on `main`, the
release carries all four archives and their `SHA256SUMS`, and `Formula/lvu.rb`
is in `indigoviolet/homebrew-tap`. Both installation paths were then verified
in clean x86_64 Linux environments against the published release.
The [release runbook](release-runbook.md) has the commands for the next
version.

## What is verified

- **The Linux archives are static musl builds** and depend on no system libc.
  The x86_64 archive was verified starting on Debian 11 (glibc 2.31), Rocky
  Linux 9 (2.34), Ubuntu 22.04 (2.35) and Alpine 3.19. See [the libc
  baseline](#the-linux-libc-baseline) for why these are not glibc builds.
- **`packaging/stage.sh` builds and verifies one archive per target.** It
  passes the triple to cargo, so an archive cannot be named for a platform it
  was not built for, and it verifies by *running* the staged tree: `--help`,
  resource resolution from an unrelated working directory and through a
  symlink, a pinned-but-empty override degrading with the right diagnostic, a
  real Polars expression compiled by the staged helper with no writes into the
  staged tree, and the staged bridge loading `@getpaseo/client` and `zod` from
  its own bundled `node_modules`. The bridge's `capabilities` exchange needs a
  reachable Paseo provider, which a build machine may not have, so that part is
  reported as a skip when the bridge fails at the transport and as a failure
  for anything else.
- **Homebrew installs and tests.** On Linuxbrew, from the published `v0.1.1`
  release with no tap, no trust record and an empty download cache:
  `brew trust indigoviolet/tap` then `brew install indigoviolet/tap/lvu`
  installs, and `brew test` passes. `brew style` and `brew audit --strict`
  report nothing on the published formula. The installed `lvu --resources`
  reports `origin: installed beside the executable` through Homebrew's `bin`
  symlink.
- **mise installs.** From the published `v0.1.1` release, in an environment
  with no credentials and a throwaway `HOME`: `mise use -g
  github:indigoviolet/lvu@0.1.1` selects the x86_64 Linux musl archive by target triple
  from the four published archives, verifies its checksum
  and attestations, discovers `bin/lvu`, and `lvu --resources` reports
  `origin: installed beside the executable`.
- **The archives carry their license text.** `LICENSE`, `LICENSE-MIT` and
  `LICENSE-APACHE` are at the archive root and under `share/doc/lvu`, and
  `stage.sh` refuses to build without them.
- **The archives download and verify unauthenticated.** From a container with
  nothing installed, `SHA256SUMS` and the Linux archive both fetch, and the
  downloaded checksum matches the published one.
- **`--help` names the packaged command.** It prints `Usage: lvu [OPTIONS]`,
  and `stage.sh` fails if `lvu-app` appears in the help at all.

## What is still unproven

- **macOS lacks interactive acceptance.** Both Darwin archives are built and
  executed on GitHub's native runners, unsigned and unnotarized. CI checks
  startup, resource resolution, a compiled Polars expression and bridge loading;
  no human terminal acceptance or PTY suite has been run there.
- **Windows is unsupported and untargeted.**
- **arm64 Linux lacks interactive and installer acceptance.** The
  `aarch64-unknown-linux-musl` archive ships in `v0.1.1`. CI builds and executes
  it natively on `ubuntu-24.04-arm`; human use and mise asset selection on
  actual arm64 hardware remain unverified.

## Targets

| Target | Runner | Status |
| --- | --- | --- |
| `x86_64-unknown-linux-musl` | `ubuntu-24.04` | shipped in `v0.1.1`, installed and verified |
| `aarch64-unknown-linux-musl` | `ubuntu-24.04-arm` | shipped in `v0.1.1`, executed in CI; no interactive acceptance |
| `aarch64-apple-darwin` | `macos-15` | shipped in `v0.1.1`, executed in CI; no interactive acceptance |
| `x86_64-apple-darwin` | `macos-15-intel` | shipped in `v0.1.1`, executed in CI; no interactive acceptance |

Every target is built on a runner of its own architecture rather than
cross-compiled, because `stage.sh` verifies an archive by executing it: a
cross-built archive could not be run on the machine that produced it, and the
job would prove nothing. Windows is not targeted; see
[portability](portability.md).

## The Linux libc baseline

A glibc archive inherits the glibc of whatever machine built it, and the symbol
versions it resolves become a hard floor. The `x86_64-unknown-linux-gnu` build
on `ubuntu-24.04` resolves up to `GLIBC_2.39` and, verified in containers,
refuses to start anywhere older:

| Distribution | glibc | glibc archive | musl archive |
| --- | --- | --- | --- |
| Ubuntu 24.04 | 2.39 | starts | starts |
| Ubuntu 22.04 | 2.35 | `version GLIBC_2.39 not found` | starts |
| Debian 12 | 2.36 | `version GLIBC_2.39 not found` | starts |
| Rocky Linux 9 | 2.34 | `version GLIBC_2.39 not found` | starts |
| Debian 11 | 2.31 | `version GLIBC_2.32 not found` | starts |

Building on an older runner would move the floor rather than remove it, and it
is not durable: `ubuntu-22.04` begins deprecation on 2026-09-17 and is
unsupported from 2027-04-17, so GitHub's retirement schedule would set lvu's
libc baseline. `x86_64-unknown-linux-musl` links statically and has no floor at
all, so that is what Linux ships. The archive name says `musl`, which is the
whole claim: no system libc is required.

Two consequences are worth knowing:

- Rust bundles a musl libc but not a C compiler for it, and Polars pulls in
  `zstd-sys` and `lz4-sys`, so a musl build needs `musl-tools`
  (`CC_x86_64_unknown_linux_musl=musl-gcc`).
- musl before 1.2.5 exports no `renameat2` wrapper, and the musl Rust bundles
  is older. lvu's three atomic renames now issue the syscall directly, so the
  glibc and musl builds make the identical kernel call.

## Resource resolution

Two optional resources live outside the executable: the pinned Python Polars
expression helper (`python/`) and the built TypeScript bridge (`bridge/`).
`crates/lvu-app/src/resources.rs` resolves each one in this order.

| # | Origin | Location |
| --- | --- | --- |
| 1 | Environment override | `LVU_PYTHON_HELPER_DIR`, `LVU_BRIDGE_DIR`, or `LVU_RESOURCE_ROOT` for a directory holding both `python/` and `bridge/` |
| 2 | Installed beside the executable | `<prefix>/libexec/lvu/<resource>`, from the executable's real path, then `<exe dir>/libexec/lvu/<resource>` |
| 3 | User data | `$XDG_DATA_HOME/lvu/libexec/<resource>`, falling back to `~/.local/share/lvu/libexec/<resource>` |
| 4 | Development checkout | the checkout this binary was built from, unchanged from previous behavior |

Rules that resolution guarantees:

- The executable path is canonicalized before step 2. Homebrew and mise both
  expose commands through symlinks, and the payload sits beside the real file.
- An override is a pinned answer. If the named directory does not contain the
  resource, lvu says so and stops; it never silently runs a resource from a
  location the operator did not name.
- A directory is accepted only when its markers exist: `pyproject.toml` and
  `lvu_expr_helper/__init__.py` for the helper, `dist/cli.js` for the bridge.
  A checked-out but unbuilt bridge is not run.
- A missing resource never panics and never blocks startup. Advanced
  expressions are left unconfigured and assistance is reported unavailable,
  each with a diagnostic naming what is missing, where lvu looked and how to
  install it. Capture, literal and `/regex/` search, native filtering,
  bookmarks, notes, Details and export all remain usable.

The precedence, the symlinked-executable case, override pinning, marker
rejection and both command forms are covered by unit tests in `resources.rs`
using temporary directories.

Check any installation with:

```sh
lvu --resources
```

## Archive layout

```
lvu-<version>-<target>/
  bin/lvu                       the real application (crate binary lvu-app)
  libexec/lvu/python/           expression helper + exported hashed lockfile
  libexec/lvu/bridge/           built bridge + production node_modules
  share/doc/lvu/                README.md, distribution.md, the licenses
  LICENSE, LICENSE-MIT, LICENSE-APACHE
```

The single top-level directory is what both installers expect to strip:
Homebrew removes it and installs the contents, and mise's `github` backend
detects a lone common prefix and copies what is inside it, so `bin/lvu` and
`libexec/lvu/` end up siblings in the install prefix either way.

The workspace's `lvu` executable is a UI demo and is deliberately never
packaged. See [packaging/README.md](../packaging/README.md) for staging,
verification and known gaps.

## Runtime prerequisites

The archive bundles the helper sources, the exported hashed lockfile, the built
bridge and its production `node_modules`. It does **not** bundle interpreters.
These are documented prerequisites rather than vendored runtimes.

| Capability | Needs | If absent |
| --- | --- | --- |
| Capture, literal and `/regex/` search, native filtering, views, bookmarks, export | nothing beyond the binary | — |
| Advanced Polars filter/enrichment expressions | `uv` on `PATH`. uv provisions CPython 3.12 and the hashed locked `polars==1.44.1` on first use, into a location uv owns. | expressions are unavailable; everything else works |
| Assistance | `node` on `PATH`, plus an installed and authenticated agent CLI | assistance is reported unavailable |

Installing or authenticating an agent provider is separate from installing lvu
and is not something lvu performs.

A packaged helper is never built in place. `uv run --project` makes setuptools
write `build/` and `.egg-info` into the install prefix, so the archive ships
`requirements.txt` exported from `python/uv.lock` with hashes, and the helper
runs as a plain package on `PYTHONPATH` with `PYTHONDONTWRITEBYTECODE=1`. The
development checkout keeps its original `mise exec -- uv run --project ...
--locked` invocation.

## End-user commands

### Homebrew

The formula is rendered from
[`packaging/homebrew/lvu.rb.in`](../packaging/homebrew/lvu.rb.in) by
[`render-formula.sh`](../packaging/homebrew/render-formula.sh), which takes
every checksum from the release's `SHA256SUMS`. No checksum is typed by hand,
and a target missing from the release is an error rather than a placeholder.
The rendered file goes to `Formula/lvu.rb` in `indigoviolet/homebrew-tap`.

```sh
brew trust indigoviolet/tap          # Homebrew 6 requires this before install
brew install indigoviolet/tap/lvu    # taps automatically
brew install uv node                 # optional: expressions, assistance
```

The trust step is not optional and is not a prompt you can answer later.
Without it, `brew install` reports `No available formula with the name
"indigoviolet/tap/lvu"`, which reads as if nothing was published. Do not run
`brew tap` before trusting: it fails with `invalid syntax in tap!` and removes
the tap it just cloned.

There are no bottles. The archives are prebuilt executables, so there is
nothing to compile and nothing to bottle.

### mise

Use the [`github` backend](https://mise.jdx.dev/dev-tools/backends/github.html).
It matches the release asset by target triple, extracts the whole archive, and
finds the executable under `bin/`, which is exactly this layout.

```sh
mise use -g github:indigoviolet/lvu
```

A release less than 24 hours old is invisible to that command: mise's
`minimum_release_age` defaults to 24h and hides newer releases from `latest`,
so a fresh release fails with "no versions found ... matching date filter".
That is the filter, not a broken release. Name the version to bypass it:

```sh
mise use -g github:indigoviolet/lvu@0.1.2
```

Upgrading later needs `mise up --bump github:indigoviolet/lvu`; `mise use -g`
records the resolved version as an exact pin, which a plain `mise up` respects.

Or in a project's `mise.toml`, optionally pinned:

```toml
[tools]
"github:indigoviolet/lvu" = "latest"   # or "0.1.2"
```

**Do not use `ubi:indigoviolet/lvu`.** ubi finds the right asset, but by
default it extracts *only* the executable it matches and discards everything
else, so `libexec/lvu/` never arrives: advanced Polars expressions and
assistance are silently unavailable. Verified against a real test release. mise
has also deprecated the ubi backend, with removal in mise 2027.1.0. If you must
use it, it needs options to keep the payload:

```sh
mise use -g "ubi:indigoviolet/lvu[extract_all=true,bin_path=bin]"
```

`mise use -g lvu` by short name would require lvu in mise's registry, which is
a pull request against jdx/mise. The explicit backend needs no registration.

mise does not provision `uv` or `node` for lvu, so a user who wants the
optional features adds them to their own tool list:

```toml
[tools]
"github:indigoviolet/lvu" = "latest"
uv = "0.12.10"
node = "26.8.1"
```

Check any installation with:

```sh
lvu --resources
```

It must report `origin: installed beside the executable`. `origin: development
checkout` means the payload was lost and lvu found a source tree instead.

## Publishing

See the [release runbook](release-runbook.md) for the exact commands: tag,
push, let `.github/workflows/release.yml` publish a draft with `SHA256SUMS`,
render the formula from those checksums, push it to the tap, undraft, and
verify both install paths. It also covers cutting the next patch release.

Two things must agree or the release is broken: the tag supplies the release
URL the formula points at, and `crates/lvu-app/Cargo.toml`'s version supplies
the archive filenames. The workflow refuses a tag push where they disagree.
