# Runnable development previews

Previews are published after useful independently tested checkpoints. User feedback
is optional and never gates ongoing implementation. Each publication has a stable
versioned local binary and a manifest with its source commit, checksum, checks and
known limitations. Previous binaries remain available while development continues.
`previews/latest` points to the most recent tested version; `mise run preview` runs it.
Binaries are local build artifacts and are not committed to Git.

## 001: TUI demo

This older version remains available at the stable executable below.
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

This older search demo remains available at the stable executable below.
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

- Working native Polars filtering and additive enrichment.
- Local source discovery and restored view memory.
- AI-assisted definitions and investigation handoff.

Each checkpoint will document its actual supported behavior and a run command.

## 003: Real local sources

`mise run preview` opens the Add source dialog. Tab switches file/command mode;
Enter starts capture. Stable executable: `previews/003-real-sources/lvu`.

```sh
./previews/003-real-sources/lvu --file /path/to/server.log
./previews/003-real-sources/lvu --command 'docker logs --follow my-container'
```

Repeat `--file` and `--command` to open several sources. Commands execute as
`sh -c` in the current working directory. Use `--capture-dir PATH` to choose
storage; the default is `.lvu-captures` in the current directory. Captured bytes
remain there after quitting. Reopening a file starts reading it again; historical
capture deduplication across acquisition runs is not implemented yet.

Keys: `n` adds a source, `[`/`]` switches views, `d` shows record details,
`f` follows arrivals, `?` shows help, and `q` quits outside editors. Escape closes
the source dialog/editor. File/command acquisition continues while browsing.

Native search and advanced filters report an explicit unsupported message in
this preview. Discovery browsing, saved view restoration, enrichment editors and
AI workflows remain pending. Multiline fragments appear as physical records;
merged time ordering and formatted timestamps are also still pending.

Primary validation: 18 UI tests, two app tests, demo PTY and real-source PTY,
startup-failure process cleanup, formatting, and clippy. Previous previews remain
unchanged. Local manifest records the exact source commit and binary checksum.
