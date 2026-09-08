# Release runbook

The exact commands that publish a release. Every step here has been rehearsed
against real archives except the two that require a published tag; those are
marked. See [distribution](distribution.md) for what the archives contain and
why the Linux target is musl, and [packaging](../packaging/README.md) for how
one archive is built and verified.

Two independent facts have to agree or the release is broken:

- the **tag** `vX.Y.Z` supplies the release URL the Homebrew formula points at
- `crates/lvu-app/Cargo.toml`'s **version** supplies the archive filenames

`.github/workflows/release.yml` refuses a tag push where those disagree, so a
mismatch fails before anything is published rather than after.

## 1. Before tagging

From a clean checkout of the commit you intend to release, on `main`:

```sh
git switch main && git pull --ff-only
git status --porcelain            # must be empty
mise run check:rust               # fmt, workspace tests, clippy -D warnings
mise run test:pty:matrix --workers 2
mise run check:bridge
```

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

Build the release archive locally once. This is the same script CI runs and it
verifies the staged tree by running it, so a local pass is real evidence:

```sh
mise exec -- packaging/stage.sh --target x86_64-unknown-linux-musl --archive
```

## 2. Tag and push

The tag must be on `main`, and `v` + the crate version:

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

If a Darwin job failed, the release still publishes with whatever archives
succeeded. That is deliberate: a Linux-only release beats no release. Note
which targets are missing; step 3 needs to know.

## 3. Render and publish the formula

Never type a checksum. `render-formula.sh` takes them from the release:

```sh
git clone https://github.com/indigoviolet/homebrew-tap ~/src/homebrew-tap
cd ~/src/homebrew-tap

gh release download v0.1.0 --repo indigoviolet/lvu -p SHA256SUMS -O - \
  | /path/to/lvu/packaging/homebrew/render-formula.sh 0.1.0 - > Formula/lvu.rb
```

If a target is genuinely absent from the release, name it — the script fails
rather than silently leaving a stale or invented checksum behind:

```sh
... | render-formula.sh 0.1.0 - \
        --allow-missing aarch64-apple-darwin \
        --allow-missing x86_64-apple-darwin > Formula/lvu.rb
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

Homebrew 6 will not load formulae from a third-party tap until you trust it,
and the trust prompt is easy to mistake for a failure:

```sh
brew tap indigoviolet/tap
brew trust indigoviolet/tap
brew install indigoviolet/tap/lvu
```

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
lost and lvu found a source tree instead. On a clean machine it will instead
report the resources missing.

Optional features need prerequisites the archive does not bundle:

```sh
brew install uv node     # or: mise use -g uv node
```

## 5. Cutting v0.1.1

```sh
git switch main && git pull --ff-only
# bump `version` in crates/lvu-app/Cargo.toml to 0.1.1
mise exec -- cargo update -p lvu-app --offline    # refresh Cargo.lock
git commit -am "lvu 0.1.1"
git push
```

Then repeat steps 1–4 with `0.1.1`. Nothing else changes: the tap formula is
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
