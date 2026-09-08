# Packaging

This directory assembles the relocatable archive that both installation
channels consume, and renders the Homebrew formula from a published release's
checksums. See [distribution](../docs/distribution.md) for what is verified and
for the end-user commands, and the [release runbook](../docs/release-runbook.md)
for the commands that publish one.

## Archive layout

```
lvu-<version>-<target>/
  bin/lvu                       the real application (the crate binary lvu-app)
  libexec/lvu/python/           expression helper + hashed exported lockfile
  libexec/lvu/bridge/           built bridge + production node_modules
  share/doc/lvu/                README.md, distribution.md, the licenses
  LICENSE, LICENSE-MIT, LICENSE-APACHE
```

`crates/lvu-app/src/resources.rs` resolves `libexec/lvu/<resource>` relative to
the real (symlink-resolved) executable, so `bin/lvu` works through Homebrew's
and mise's link farms. The workspace's `lvu` executable is a UI demo and is
never packaged; the archive's `bin/lvu` is `lvu-app`.

All three license files are staged, at the archive root and under
`share/doc/lvu`. `stage.sh` refuses to build an archive when one is missing;
the crates declare `MIT OR Apache-2.0` and an archive must carry what it
claims.

The packaged helper carries `requirements.txt`, exported from `python/uv.lock`
with hashes at staging time. Its presence tells the app to run the helper as a
plain package on `PYTHONPATH` instead of `uv run --project`, which would make
setuptools write `build/` and `.egg-info` into the directory the package
manager owns. `stage.sh` asserts that a real expression compile leaves the
staged tree byte-identical.

## Building and verifying locally

```sh
mise run install:bridge               # tsc and the test runner
mise run check:bridge                 # dist/cli.js must exist before staging
mise exec -- packaging/stage.sh --target x86_64-unknown-linux-musl --archive
```

Options: `--profile dev` (much faster than a release Polars build),
`--target TRIPLE`, `--out DIR`, `--skip-bridge`, `--archive`.

`--target` is passed to cargo, not just used as a filename, so an archive
cannot be named for a platform it was not built for. Because verification runs
the staged binary, each target must be built on a machine that can execute it;
a build that cannot run says so rather than looking like a broken archive. The
Linux target is `x86_64-unknown-linux-musl` and needs `musl-tools` installed
with `CC_x86_64_unknown_linux_musl=musl-gcc`.

Verification runs against the staged tree, from a working directory unrelated
to both the checkout and the staging root:

1. `bin/lvu --help`, which must print `Usage: lvu [OPTIONS] ...` and must not
   name the crate binary `lvu-app` anywhere
2. `bin/lvu --resources` directly and through a symlink; both must report
   `origin: installed beside the executable` and must not name the checkout
3. a pinned but empty `LVU_RESOURCE_ROOT` must report the actionable missing
   diagnostic and must not fall through to another location
4. the staged helper compiles a real Polars expression and writes nothing into
   the staged tree
5. the staged bridge resolves `@getpaseo/client` and `zod` from its own
   bundled `node_modules`, then answers `capabilities`. `node dist/cli.js`
   connects to a Paseo provider before serving anything, so a machine without
   one reports a skip naming the transport error rather than failing; a missing
   module or a syntax error still fails.

Steps 4 and 5 are skipped, with a message, when `uv` or `node` is absent.

Because the workspace target directory may be shared with other checkouts, the
script copies the freshly built binary immediately after `cargo build`.

## Contents

| Path | Purpose |
| --- | --- |
| `stage.sh` | Build, stage and verify one target's archive. |
| `homebrew/lvu.rb.in` | Formula template. Not loadable; every checksum is a token. |
| `homebrew/render-formula.sh` | Renders the tap formula from a release's `SHA256SUMS`. |

`.github/workflows/release.yml` builds the per-target archives and publishes a
draft release with `SHA256SUMS`.

## Known gaps

- macOS archives are unsigned and unnotarized, and no Darwin binary has been
  run on a Mac. They are built on GitHub runners only.
- Linux is x86_64 only; there is no `aarch64-unknown-linux-musl` archive.
