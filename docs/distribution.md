# Installation and distribution

Status: 2026-09-07. **No lvu release, tag, GitHub release, tap repository or
mise registry entry exists.** Nothing in this document can be installed today.
What does exist is the packaging machinery and the relocatable resource
resolution that previously made installation impossible. Building from the
source checkout remains the only working path; see the [README](../README.md).

## What works now

- The application resolves its optional runtime resources relative to the
  running executable instead of the build checkout, so a copied or installed
  tree is a complete installation. Precedence and diagnostics are below.
- `packaging/stage.sh` builds and stages a relocatable archive per target and
  verifies the staged tree: `--help` and resource resolution from an unrelated
  working directory and through a symlink, a real Polars expression compiled by
  the staged helper with no writes into the staged tree, and the staged bridge
  answering `capabilities`. Verified on Linux x86_64 with a dev-profile build.
- `lvu --resources` reports what resolved, from where, and the exact command
  each resource will run.

## What is still pending

- No release, so no archive, no checksum, no `brew install`, no `mise use`.
  The Homebrew formula's `url` and `sha256` values are placeholders.
- `.github/workflows/release.yml` has never run. The repository has no CI
  history; treat the workflow as reviewed intent, not evidence.
- macOS and Windows are unvalidated. Evidence is Linux x86_64 only.
- No `LICENSE` file is committed, so archives ship no license text.
- Linux archives inherit the build machine's glibc; no libc baseline or musl
  target has been chosen. macOS archives would be unsigned and unnotarized.
- `--help` still prints `Usage: lvu-app` although the packaged command is `lvu`.

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
  expressions are left unconfigured and 🧠 assistance is reported unavailable,
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
  share/doc/lvu/                README.md, distribution.md
```

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
| 🧠 assistance | `node` on `PATH`, plus an installed and authenticated agent CLI | assistance is reported unavailable |

Installing or authenticating an agent provider is separate from installing lvu
and is not something lvu performs.

A packaged helper is never built in place. `uv run --project` makes setuptools
write `build/` and `.egg-info` into the install prefix, so the archive ships
`requirements.txt` exported from `python/uv.lock` with hashes, and the helper
runs as a plain package on `PYTHONPATH` with `PYTHONDONTWRITEBYTECODE=1`. The
development checkout keeps its original `mise exec -- uv run --project ...
--locked` invocation.

## Intended end-user commands

Neither command works yet; both are recorded so the first release only has to
publish artifacts.

### Homebrew

Formula: [`packaging/homebrew/lvu.rb`](../packaging/homebrew/lvu.rb), intended
for a dedicated `indigoviolet/homebrew-tap` repository rather than an initial
homebrew/core submission. Follow the official
[tap workflow](https://docs.brew.sh/How-to-Create-and-Maintain-a-Tap).

```sh
brew install indigoviolet/tap/lvu
brew install uv node   # optional: advanced expressions, 🧠 assistance
```

Publishing checklist: tag a release, upload the archives, replace
`VERSION_PLACEHOLDER` and every `SHA256_PLACEHOLDER_*` with published
checksums, commit a `LICENSE`, then run `brew audit --strict --new lvu` and
`brew test lvu`. Do not invent checksums; take them from the release's
`SHA256SUMS`.

### mise

mise's [GitHub backend](https://mise.jdx.dev/dev-tools/backends/github.html)
matches release assets by platform and finds an executable under `bin/`, which
is exactly the archive layout. An explicit backend works without registering
the short name.

```sh
mise use -g github:indigoviolet/lvu
```

Or in a project's `mise.toml`:

```toml
[tools]
"github:indigoviolet/lvu" = "latest"
```

Pinning a specific release:

```toml
[tools]
"github:indigoviolet/lvu" = "0.1.0"
```

mise installs the archive and links `bin/lvu` into its shim directory; the
resolution above follows the link back to the real payload. mise does **not**
provision `uv` or `node` for lvu, so a user who wants the optional features
adds them to their own tool list:

```toml
[tools]
"github:indigoviolet/lvu" = "latest"
uv = "0.12.10"
node = "26.8.1"
```

Verify with a clean mise data directory before advertising this command:
confirm asset matching by platform, executable discovery under `bin/`, and that
`lvu --resources` reports `origin: installed beside the executable`.

## Release sequence

1. Choose a Linux libc baseline and decide whether to ship musl. Do not label a
   locally built executable universally portable.
2. Add `LICENSE` files and include them in the archive.
3. Run `.github/workflows/release.yml` with `dry_run` and fix what breaks; it
   has never executed.
4. Tag, let the workflow publish a draft release with `SHA256SUMS`, and check
   cold install, upgrade, version reporting, files/stdin/commands, terminal
   restoration, Unicode and copy, and the optional helper and bridge paths on a
   machine with no checkout.
5. Substitute the real checksums into the formula, create the tap, and exercise
   both `brew install` and `mise use` before advertising either.
6. Keep local preview numbering distinct from public release versions, and
   carry the schema-v4 compatibility note into release notes.
