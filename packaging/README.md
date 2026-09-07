# Packaging

This directory assembles the relocatable archive that both installation
channels consume. Nothing here has been published: there is no lvu release,
tag, tap or registry entry yet. See [distribution](../docs/distribution.md) for
current status and end-user commands.

## Archive layout

```
lvu-<version>-<target>/
  bin/lvu                       the real application (the crate binary lvu-app)
  libexec/lvu/python/           expression helper + hashed exported lockfile
  libexec/lvu/bridge/           built bridge + production node_modules
  share/doc/lvu/                README.md, distribution.md
```

`crates/lvu-app/src/resources.rs` resolves `libexec/lvu/<resource>` relative to
the real (symlink-resolved) executable, so `bin/lvu` works through Homebrew's
and mise's link farms. The workspace's `lvu` executable is a UI demo and is
never packaged; the archive's `bin/lvu` is `lvu-app`.

The packaged helper carries `requirements.txt`, exported from `python/uv.lock`
with hashes at staging time. Its presence tells the app to run the helper as a
plain package on `PYTHONPATH` instead of `uv run --project`, which would make
setuptools write `build/` and `.egg-info` into the directory the package
manager owns. `stage.sh` asserts that a real expression compile leaves the
staged tree byte-identical.

## Building and verifying locally

```sh
mise run check:bridge                 # dist/cli.js must exist before staging
mise exec -- packaging/stage.sh --archive
```

Options: `--profile dev` (much faster than a release Polars build),
`--target TRIPLE`, `--out DIR`, `--skip-bridge`, `--archive`.

Verification runs against the staged tree, from a working directory unrelated
to both the checkout and the staging root:

1. `bin/lvu --help`
2. `bin/lvu --resources` directly and through a symlink; both must report
   `origin: installed beside the executable` and must not name the checkout
3. a pinned but empty `LVU_RESOURCE_ROOT` must report the actionable missing
   diagnostic and must not fall through to another location
4. the staged helper compiles a real Polars expression and writes nothing into
   the staged tree
5. the staged bridge answers `capabilities` from its bundled dependencies

Steps 4 and 5 are skipped, with a message, when `uv` or `node` is absent.

Because the workspace target directory may be shared with other checkouts, the
script copies the freshly built binary immediately after `cargo build`.

## Contents

| Path | Purpose |
| --- | --- |
| `stage.sh` | Build, stage and verify one target's archive. |
| `homebrew/lvu.rb` | Tap formula. Every `url`/`sha256` is a placeholder. |

`.github/workflows/release.yml` builds the per-target archives and publishes a
draft release with `SHA256SUMS`. It has never run.

## Known gaps

- No `LICENSE` file exists in the repository, so the archive ships none. The
  crate declares `MIT OR Apache-2.0`; add the texts before publishing.
- `--help` still prints `Usage: lvu-app` even though the packaged command is
  `lvu`.
- Linux archives are built against the runner's glibc. No explicit libc
  baseline or musl target is chosen yet; do not call them universally portable.
- macOS archives are unsigned and unnotarized.
