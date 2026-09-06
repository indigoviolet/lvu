# Installation and distribution plan

Status: exploration, 2026-09-06. Main is pushed to GitHub; Homebrew and mise
release installation are not available yet. Local preview binaries are ignored
and are not GitHub release assets.

## Proposed user paths

Publish versioned, checksummed platform archives through GitHub Releases.
The intended mise command after an accepted release is:

```sh
mise use -g github:indigoviolet/lvu
```

Mise's [GitHub backend](https://mise.jdx.dev/dev-tools/backends/github.html)
selects uploaded release assets by platform and can locate a binary under `bin/`.
An explicit backend works without first registering the short name `lvu`.
Verify asset matching and executable discovery with a clean mise data directory.
Do not advertise this command as working until release assets exist.

For Homebrew, start with a dedicated `indigoviolet/homebrew-tap` repository and
an `lvu` formula, rather than an initial homebrew/core submission. The intended
command is `brew install indigoviolet/tap/lvu`. The tap does not exist as part of
this work. Follow the official [tap workflow](https://docs.brew.sh/How-to-Create-and-Maintain-a-Tap)
for formula testing and bottles. Homebrew supports open-source CLI programs as
[formulae](https://docs.brew.sh/Adding-Software-to-Homebrew).

## Packaging work required first

The current main binary is named `lvu-app`; the separate `lvu` binary is a demo.
Release archives must expose the real app as `bin/lvu`, never ship the demo under
that command by accident.

`compiler_config()` and `agent_config()` in `crates/lvu-app/src/main.rs` currently
resolve resources from compile-time `CARGO_MANIFEST_DIR`. The compiler launches
mise/uv against the checkout's Python project; the bridge launches pinned Node
against the checkout's built TypeScript. A copied binary is therefore not a
portable full installation.

Use one relocatable package layout for both channels: `bin/lvu` plus versioned
helper resources under `libexec/lvu/`. Resolve installed resources relative to the
real executable/install prefix, with a deliberate development fallback. Python
helper and bridge versions must match the app. Do not put captures or mutable
runtime environments inside the package manager's installation directory.

Decide and validate runtime provisioning before promising a one-command full
install: the expression helper currently requires Python 3.12 and Polars 1.44.1;
the bridge uses Node 26.8.1 and locked npm dependencies. A Homebrew formula can
provision dependencies and a private helper environment. A mise GitHub archive
alone does not provision those external runtimes. Either bundle compatible
runtimes per platform or provide an explicit, versioned setup step for optional
expression/assistance features. Ordinary browsing and literal/regex operations
must remain usable when optional helpers are absent. Provider installation and
user authentication remain separate from installing lvu.

## Release and acceptance sequence

1. Implement relocatable helper discovery and verify a moved installation with
   the build checkout unavailable. Test paths containing spaces and symlinks.
2. Build immutable archives with the real app, required helper resources,
   licenses and checksums. Choose an explicit Linux libc compatibility baseline;
   do not label a locally built executable universally portable.
3. Validate Linux x86_64 first. Add macOS arm64/x86_64 and other targets only after
   actual build and terminal acceptance. Current evidence is Linux-only.
4. Check cold install, upgrade, version reporting, files/stdin/commands, terminal
   restoration, Unicode/copy, and the optional Python/bridge paths without a
   checkout. Test schema-v4 compatibility messaging and preserved user data.
5. Exercise the release archive through mise and the formula through Homebrew,
   then publish a versioned release and the tap. Keep local preview numbering
   distinct from public release versions.

No release tag, GitHub release, tap repository, workflow or install command was
created or executed during this exploration. Building from the source checkout
remains the available GitHub path; see the README.
