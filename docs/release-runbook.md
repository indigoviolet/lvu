# Release runbook

All releases use the existing annotated `vX.Y.Z` version scheme. There are no
preview releases, preview tags or separate development-release numbers. The tag,
app crate version and lockfile version must agree. Push the specific version tag
rather than all local tags, and never move an existing version tag.
See [versions](versions.md) for local installation and retention.

Supported release targets are Linux x86_64/arm64 and Apple-silicon macOS.
Intel Macs and Windows are unsupported. The release workflow and default
Homebrew renderer require this three-target set. Historical four-archive
releases remain immutable; `--with-intel-darwin` is available only when
re-rendering a historical formula.

**State: `v0.1.6` is the latest published release, with four historical
archives. `v0.1.7` is an immutable draft with a known restore defect and must
never be published, retagged or have its assets replaced.** The corrective next
release is `v0.1.8` and uses three targets. Steps 1–4 retain `v0.1.0` command
examples; substitute the new version when following
[step 5](#5-cutting-the-next-version).
Do not recreate or move a published tag. Historical installation evidence is
described in [distribution](distribution.md); local fixture checks of the
three-target change are recorded in the [work ledger](work-ledger.md).

Two independent facts have to agree or the release is broken:

- the **tag** `vX.Y.Z` supplies the release URL the Homebrew formula points at
- `crates/lvu-app/Cargo.toml`'s **version** supplies the archive filenames

`.github/workflows/release.yml` refuses a tag push where those disagree, so a
mismatch fails before anything is published rather than after.

## 1. Before tagging

From a clean checkout of the commit you intend to release, on the validated release commit:

```sh
git status --short --branch # use the reviewed integration/release checkout
git status --porcelain            # must be empty
mise run check:rust               # fmt, workspace tests, clippy -D warnings
mise run test:pty:matrix          # every PTY suite, concurrently
mise run install:bridge           # tsc and vitest; a bare checkout has neither
mise run check:bridge
```

The matrix task already sets its own concurrency. Appending `--workers 2` does
work, but only because the task passes `--workers 4` first and argparse keeps
the last value; that is a coincidence of argument order, not an interface. Pass
it only when you mean to override, and expect nothing from it otherwise.

**The full PTY matrix has not been accepted on macOS.** The dedicated arm64
macOS workflow has passed installed-resource, kernel-PTY, terminal-restoration
and process-cleanup checks; see `docs/platform-validation.md` for the exact run.
That bounded result does not cover every suite or a human terminal emulator.
The portability audit expects the terminal-capability assertions
(SGR mouse, OSC 52, truecolor) to fail under Terminal.app. So if you are
cutting a release from a Mac, do not treat a red matrix there as a release
blocker and do not treat a green one as acceptance. Either run this step on the
Linux machine, or skip it and rely on what CI proves: the release workflow runs
`packaging/stage.sh` on every target, and stage.sh verifies by executing the
staged binary. That is strictly weaker -- an archive check, not a terminal
check -- so record it as such rather than claiming the suites passed.

The release workflow deliberately runs only `install:bridge` and
`build:bridge`, not `check:bridge`: the bridge's vitest suite asserts 50ms
operation budgets that a loaded shared runner misses. Running the full check
here, on a machine you control, is where that suite belongs.

Confirm the version that will name the archives:

```sh
sed -n 's/^version = "\(.*\)"/\1/p' crates/lvu-app/Cargo.toml | head -1
```

Every document that tells a user how to install must name the same mise
backend, which is `github:` and never `ubi:` (see step 4 for why):

```sh
# Every `mise use -g` line for lvu must name the github backend. A bare ubi:
# backend installs the executable without libexec/, and mise has deprecated it.
grep -rn 'mise use -g.*indigoviolet/lvu' README.md docs/ \
  | grep -v 'github:' | grep -v 'extract_all' | grep -v 'grep -rn' \
  && echo "^ these recommend a backend that drops libexec/" \
  || echo "every install command names the github backend"
```

Rehearse the release archive locally when the host has the target toolchain and
build space. CI must build and execute each native archive before publication;
that evidence can supply packaging acceptance on this disk-constrained host.
The local rehearsal uses the same script:

```sh
mise exec -- packaging/stage.sh --target x86_64-unknown-linux-musl --archive
```

Rehearse only the target this machine can run. `stage.sh` executes the archive
it stages, so `aarch64-unknown-linux-musl` is rehearsed on an arm64 Linux
machine or by CI, never cross-built from x86_64.

**That command is Linux-only.** stage.sh passes `--target` straight to cargo,
so the musl archive needs the `x86_64-unknown-linux-musl` target installed
(`rustup target add`) and a musl C toolchain for Polars' `zstd-sys` and
`lz4-sys` (`musl-tools`, with `CC_x86_64_unknown_linux_musl=musl-gcc`). A stock
Mac has neither, and stage.sh verifies an archive by running it, so it would
refuse the result anyway. On macOS, rehearse the host's own archive by omitting
`--target`:

```sh
mise exec -- packaging/stage.sh --archive     # defaults to the host triple
```

That rehearses the Darwin archive, which is the one that machine can actually
run. The Linux archive is then rehearsed by CI only, on `ubuntu-24.04` -- the
same script on the same target, but CI's evidence rather than yours.

## 2. Tag and push

The tag must identify the exact validated release commit and equal `v` + the crate version:

```sh
git tag -a v0.1.0 -m "lvu 0.1.0"
git push origin v0.1.0
```

Pushing the tag starts `release.yml`. It builds and verifies one archive per
target and then publishes a **draft** release carrying the archives and
`SHA256SUMS`. Nothing is public until you undraft it.

```sh
gh run watch "$(gh run list --workflow release.yml --limit 1 --json databaseId --jq '.[0].databaseId')"
gh release view v0.1.0
```

If any native job fails, keep the release draft and resolve the failure.
Publication requires all three supported archives and their checksums.
A partial draft must remain a draft; do not update the tap or publish it.

## 3. Render and publish the formula

Never type a checksum. `render-formula.sh` takes them from the release:

```sh
git clone https://github.com/indigoviolet/homebrew-tap ~/src/homebrew-tap
cd ~/src/homebrew-tap

gh release download v0.1.0 --repo indigoviolet/lvu -p SHA256SUMS -O - \
  | /path/to/lvu/packaging/homebrew/render-formula.sh 0.1.0 - > Formula/lvu.rb
```

The renderer supports explicitly missing targets for historical partial releases.
The example below is historical only; never use missing-target exceptions
for a new release. Re-rendering an older Intel-inclusive formula requires
`--with-intel-darwin`:

```sh
gh release download v0.1.0 --repo indigoviolet/lvu -p SHA256SUMS -O - \
  | /path/to/lvu/packaging/homebrew/render-formula.sh 0.1.0 - \
      --with-intel-darwin \
      --allow-missing aarch64-apple-darwin \
      --allow-missing x86_64-apple-darwin > Formula/lvu.rb
```

The default renderer requires `x86_64-unknown-linux-musl`,
`aarch64-unknown-linux-musl` and `aarch64-apple-darwin`. It excludes Intel macOS
even when an old checksum file lists that archive. The explicit historical flag
also enables `x86_64-apple-darwin`. Dropping every target under a platform removes that
platform's whole block, so a release with no Linux archive renders a
macOS-only formula rather than an empty `on_linux`.

```sh
```

Check it before pushing:

```sh
brew style Formula/lvu.rb
brew audit --strict --formula indigoviolet/tap/lvu
```

Then publish the formula and undraft the release:

```sh
git add Formula/lvu.rb
git commit -m "lvu 0.1.0"
git push
gh release edit v0.1.0 --repo indigoviolet/lvu --draft=false
```

Undrafting last matters: until the release is public the formula's URLs 404,
so nobody can pull a half-published tap.

## 4. Verify both install paths

On a machine with no lvu checkout.

### Homebrew

Homebrew 6 will not load formulae from a third-party tap until you trust it.
Trust **before** installing, and do not run `brew tap` first:

```sh
brew trust indigoviolet/tap
brew install indigoviolet/tap/lvu    # taps automatically
```

Both halves of that matter, and both were verified from a clean Homebrew:

- Without the trust, `brew install indigoviolet/tap/lvu` taps and then says
  `Warning: Skipping indigoviolet/tap because it is not trusted`, followed by
  `No available formula with the name "indigoviolet/tap/lvu"` and a spelling
  suggestion. It looks like the formula was never published.
- Running `brew tap indigoviolet/tap` before trusting is worse: it tries to
  load every formula in the tap, refuses each one, reports
  `Error: Cannot tap indigoviolet/tap: invalid syntax in tap!` and deletes the
  tap it just cloned. The tap is fine; it was untrusted.

`brew trust` records the tap name, so it works before the tap exists locally
and `brew install` then taps cleanly.

If `brew trust` answers `Unknown command: trust`, you are on Homebrew 5 or
older, which has no tap-trust mechanism. Skip that line: `brew install
indigoviolet/tap/lvu` is the whole command there.

### mise

Use the `github` backend, **not** `ubi`. Both find the right archive, but ubi
by default extracts only the executable and throws away `libexec/`, so the
Polars helper and the assistance bridge are silently lost. mise also deprecated
the ubi backend, with removal in mise 2027.1.0.

```sh
mise use -g github:indigoviolet/lvu
```

**For the first 24 hours after publishing, that exact command fails**, and it
fails in a way that reads like a broken release:

```
mise ERROR Failed to install github:indigoviolet/lvu@latest:
  no versions found for github:indigoviolet/lvu matching date filter
```

mise's `minimum_release_age` defaults to 24h and hides releases newer than
that from any `latest` resolution, as a supply-chain precaution. Nothing is
wrong with the release. Verify with an explicit version, which is not filtered:

```sh
mise use -g github:indigoviolet/lvu@0.1.0
```

or, to check the exact command users will run, waive the filter for one
invocation:

```sh
MISE_MINIMUM_RELEASE_AGE=0 mise use -g github:indigoviolet/lvu
```

Both were verified against a real release published minutes earlier. Do not
announce the unversioned command until the release is a day old, or say
alongside it that a brand-new release needs the pinned form.

If you must use ubi, it needs options to keep the payload:

```sh
mise use -g "ubi:indigoviolet/lvu[extract_all=true,bin_path=bin]"
```

### Both

```sh
cd /tmp
lvu --help          # must say "Usage: lvu [OPTIONS] [FILE ...]"
lvu --resources     # must say "origin: installed beside the executable"
```

`--resources` reporting `origin: development checkout` means the payload was
lost and lvu fell through to a source tree that happened to be on the machine.
The two sentences describe one fault with two symptoms: on a machine with no
checkout, that same lost payload has nothing to fall through to, so it reports
the resources *missing* instead. `missing` is what a user would actually see;
`development checkout` is what you see when testing on a build machine.

Optional features need prerequisites the archive does not bundle:

```sh
brew install uv node     # or: mise use -g uv node
```

## 5. Cutting the next version

`v0.1.6` is published. `v0.1.7` remains an immutable unpublished draft because
its restore defect was confirmed after archive creation. Use `0.1.8` for the
next release only after its integrated features pass the checks above; never
move or reuse the `v0.1.7` tag, and do not replace or publish its draft assets.

The next release requires verified Linux x86_64, Linux arm64 and Apple-silicon
macOS archives and checksums. Intel Darwin is excluded. Run
`mise exec -- bash packaging/homebrew/tests/test-render-formula.sh` when changing
formula generation; local fixtures do not replace actual archive acceptance.

```sh
git status --short --branch # use the reviewed integration/release checkout
# bump `version` in crates/lvu-app/Cargo.toml to 0.1.8
mise exec -- cargo update -p lvu-app --offline    # refresh Cargo.lock
# Any build or `cargo check` refreshes it just as well. The point is only that
# Cargo.lock must record the new version before you commit, or the release
# build fails on --locked.
git commit -am "lvu 0.1.8"
git push
```

Then repeat steps 1–4 with `0.1.8`. The tap formula is
re-rendered from the new release's `SHA256SUMS` and overwrites the old one,
because `render-formula.sh` writes the whole file.

Users upgrade with:

```sh
brew update && brew upgrade lvu
mise up --bump github:indigoviolet/lvu
```

`mise up` without `--bump` does nothing here: `mise use -g` writes the resolved
version into the config as an exact pin, and a plain upgrade respects it.
`--bump` moves the pin. It is also subject to the same 24h release-age filter,
and says so plainly when it declines:

```
mise WARN newer ... release 0.1.1 (released ..., eligible ...) ignored by
  minimum_release_age (24h); no eligible release found
```

## Changelog

Keep [`CHANGELOG.md`](../CHANGELOG.md) current as accepted changes land:
add them under `Unreleased` in the Features / Fixes / Breaking changes
groups. At release, move that section under the exact version heading and
date, and carry the same categories into the published release notes.
Ongoing proposals stay in [`TODO.md`](../TODO.md), never as shipped claims.

## What is not automated, and why

- **Undrafting the release** is manual. The workflow publishes a draft so a
  failed Darwin runner or a bad archive can be caught before anything is
  public.
- **Pushing the formula** is manual. The tap is a separate repository, and a
  formula pushed before the release is undrafted points at URLs that 404.
- **macOS signing and notarization** do not happen. The Darwin archives are
  unsigned. Homebrew installs from its own download, so no Gatekeeper
  quarantine attribute is set and `brew install` works; a manually downloaded
  archive would be quarantined and need `xattr -d com.apple.quarantine`.
- **The mise registry short name.** `mise use -g lvu` would need lvu added to
  mise's registry, which is a pull request against jdx/mise. Until then the
  explicit `github:indigoviolet/lvu` backend is the supported form and needs
  no registration.
