# Runnable development previews

Previews are published after useful independently tested checkpoints. User feedback
is optional and never gates ongoing implementation. Each publication has a stable
versioned local binary and a manifest with its source commit, checksum, checks and
known limitations. Previous binaries remain available while development continues.
`previews/latest` points to the most recent tested version; `mise run preview` runs it.
Binaries are local build artifacts and are not committed to Git.

## 001: TUI demo

Run from the repository: `mise run preview`.
Stable executable: `previews/001-tui-demo/lvu --demo`.
Source commit: `852a855825d67c708009ced6415dcd7bf990e400`.

Synthetic logs only. Supports switching views, navigation, mouse selection/scroll,
details, follow/history, resizing and an editable filter draft. The query adapter
is unwired; filters do not execute. Real file/command sources are not connected.

Keys: `?` help, `q` quit, `[`/`]` views, `d` details, `f` follow,
`/` filter draft, `a` synthetic arrival. Arrow keys and mouse scrolling also work.

Primary independently reran ten Rust state/UI tests and the real PTY suite,
including viewport contents, input, resize, and normal/panic terminal restoration.
This preview does not imply acceptance of the complete production integration.

## 002: Live literal search

`mise run preview` now launches this version.
Stable executable: `previews/002-text-search/lvu --demo`.
Implementation: `e07bd8d` (owner `5a3974c`).

Press `/` and type `request 05`, `café`, or `late fixture` to narrow demo logs.
Search updates after 120 ms of idle typing. Punctuation is literal; case matching
uses Unicode lowercase. Press Escape to return to navigation. Reopen `/` and
backspace to empty to restore all rows. `a` supplies synthetic arrivals that obey
the active search. Views retain independent search state.

`p` opens the separate advanced Polars editor; execution remains unwired in this
preview. Real sources are still being integrated. Preview 001 remains available.

Primary reran 13 TUI tests and the actual PTY workflow after integration, including
search/no-match/clear/arrival behavior and normal/panic terminal restoration.

## Upcoming checkpoints

- Real file and command capture in the viewer.
- Working native Polars filtering and additive enrichment.
- Local source discovery and restored view memory.
- AI-assisted definitions and investigation handoff.

Each checkpoint will document its actual supported behavior and a run command.
