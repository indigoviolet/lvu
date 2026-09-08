# Development notes

The [README](../README.md) introduces the product. This document covers building,
validation and repository conventions; in-app Help and the command palette are
the user-facing reference for controls.

## Build and run

Use mise for pinned Rust, Python, Node and uv versions. From a fresh clone:

```sh
mise trust
mise install
mise run doctor
mise run build:app
./target/debug/lvu-app app.log
```

`lvu-app` is the real-source application. The workspace's `lvu` executable is a
synthetic demo used by UI tests; don't package it as the real app.

Local development checkouts can use `mise run preview` when separately published
preview binaries are present. The ignored `previews/` directory is not included
in a clone. Preview publication records source revision, checksum and copied-app
acceptance; see [preview notes](previews.md).

## Optional expression and agent helpers

Python constructs supported Polars expressions; Rust executes them. The pinned
Python/Rust Polars pair and locked helper environment are a compatibility
contract. Do not upgrade one side independently. Literal and regex operations do
not require Python startup.

Build the TypeScript adapter for optional assistance:

```sh
mise run install:bridge
mise run check:bridge
```

A configured local provider is required for actual model calls. The adapter is
implemented through Paseo; this name is an implementation detail rather than a
product control label. See [bridge setup](../bridge/README.md).

The app currently resolves helpers from its build checkout. A copied binary is
not a portable full installation. See [distribution](distribution.md) before
packaging or advertising an install command.

## Validation

```sh
mise run check:rust
mise run check:expr
mise run check:bridge
mise run test:pty
mise run test:pty:real
```

Focused PTY tasks cover search, enrichment, stdin, gzip, themes/settings, copying,
merged views and other workflows. Use `mise tasks` to find them. Expression and
PTY tasks use locked Python environments through uv.

Keep Polars compilation concurrency bounded and debug information/incremental
builds disabled on the shared development host. Coordinate target ownership with
other agents before compiling. Preserve captures, prior previews and proof
archives; recover space only through targeted build-tool cleanup.

UI changes need both TestBackend checks and actual terminal interaction. Test
normal exit and panic restoration where terminal behavior changes. Distinguish
component tests from integrated application acceptance.

## Preferences and storage reference

Preferences default to `$XDG_CONFIG_HOME/lvu/settings.toml`, with
`~/.config/lvu/settings.toml` as fallback. See [settings.example.toml](settings.example.toml).
New model settings default to `codex/gpt-5.6-luna`; saved choices and explicit
environment overrides take precedence.

`LVU_REDUCED_MOTION`, `LVU_NO_DELIGHT` and `LVU_ASCII` are presence-based flags,
including empty values and `0`. They select static animation, disabled visual
accents and ASCII rendering respectively.

Durable data defaults to `$XDG_DATA_HOME/lvu` and disposable indexes to
`$XDG_CACHE_HOME/lvu`, falling back to `~/.local/share/lvu` and `~/.cache/lvu`.
`--capture-dir` overrides the durable root. A legacy `.lvu-captures` directory may
be selected with a notice; it is not moved automatically. Cache settings take
effect after restart. See architecture for exact ownership and budget semantics.

Startup re-acquires the previous session's sources by default. The set lives in
`<capture-dir>/workspace/session.json`, beside the workspace database but not in
it: it needs no schema version and an older binary simply ignores it. `--resume`
names the default explicitly, `--fresh` acquires nothing while leaving every
capture and saved view in place, and source arguments are added to the resumed
set, deduplicated by the definition's content-addressed source id. A resumed
command is launched through the same path as the sidebar's Restart, including a
command that exited nonzero last session. Stdin cannot be re-acquired and its
source appears in the sidebar not acquiring anything.

Workspace schema v4, introduced with preview034, cannot be opened by preview033
or earlier after migration. Keep this compatibility information in release notes.

## Before changing code

Read [AGENTS.md](../AGENTS.md), [architecture](architecture.md),
[contracts](contracts.md), [TODO](../TODO.md) and current [work ledger](work-ledger.md)
entries. The [implementation plan](implementation-plan.md) includes proposed and
historical designs; it is not evidence of implemented features.

Keep development history in the ledger, open work in TODO and binary-specific
validation in preview notes. Keep the README focused on supported user benefits.
