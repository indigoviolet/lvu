# lvu — Love You Log Time

A local terminal workspace for logs. Capture files and processes once, search and
progressively extract fields, keep independent views, and investigate fixed data
snapshots with optional 🧠 assistance.

The current published build is **preview032**. Features below describe that build;
[preview notes](docs/previews.md) record release evidence and known limits.
See [TODO.md](TODO.md) for remaining work and reported bugs, and
[the architecture](docs/architecture.md) for the implementation map.

## Run

```sh
mise trust
mise install
mise run doctor
mise run preview app.log worker.log
mise run preview --command 'docker logs -f api'
producer | mise run preview
```

`mise run preview --help` lists source options. Files are positional; `--file` is
also supported. `--command`/`-c` are repeatable shell commands. `--stdin` or `-`
explicitly selects stdin; redirected stdin is detected automatically. Use `--`
before file names beginning with a dash. `--capture-dir` selects a capture workspace.

Without CLI sources, the pixel-art **LOVE YOU LOG TIME** title stays until Escape.
CLI sources skip it. Ctrl-P opens the searchable command palette with shortcuts;
`?` opens help.

## Supported features

| Area | Features |
| --- | --- |
| Capture | Plain files with follow/resume, rotation/truncation boundaries, command stdout/stderr, piped or redirected stdin, transparent gzip file detection and decompression. Exact bytes and physical record identities are retained. |
| Sources | Manual addition (`n`), file-path completion, explicit stop/restart, Docker/Linux process/project discovery, remembered sources and reviewed 🧠 source proposals. Discovery never launches candidates automatically. |
| Views | Independent named views (`v`), blank/clone/rename, per-view settings, and ordered merged views over already-open sources (`v`, Alt-M). Shared views reuse capture. |
| Search | Debounced raw-text, field-text and regex search (`/`), plus Polars Boolean expressions. Separate advanced filtering (`p`) combines with other constraints using AND. |
| Enrichment | Ordered additive stages (`e`), named regex captures, Polars expressions, dependencies on earlier outputs, edit/remove and full rollback on invalid changes. |
| Inspection | Details (`d`), field picker/pinned columns (`i`), severity and stable value colors, raw neighboring context (`o`), bookmarks (`b`) and notes/browser (`B`). |
| Navigation | Follow/history, horizontal event scrolling, reversible multiline/stack-trace grouping (`m`), capture/event/extracted timestamp windows (`t`) and rolling presets. |
| Recipes | Named reusable settings (`r`), import/export, immutable revision history, explicit updates, similar-source suggestions and reviewed 🧠 adaptation. |
| Assistance | Reviewed filter/enrichment proposals (`A`), timestamp assistance inside Time, and separate resumable investigations (`I`) over fixed Parquet snapshots. Raw browsing works without a model connection. |
| Interface | Command palette, field/value autocomplete, visible input cursors, themed dialogs, shaded pixel title, heartbeat activity indicator, and visible-text drag/copy using Ctrl-C and terminal OSC 52. |
| Persistence | Accepted constraints and independent unfinished drafts, view names, navigation, presentation, bookmarks, recipes and investigation metadata survive restart. Remembered commands require explicit launch. |
| Settings/storage | Global XDG TOML settings (`,`), model preferences, six themes, motion/ASCII options, memory/index budgets, storage usage (`S`) and reviewed unused-index cleanup. |

### Search and enrichment examples

In Search (`/`):

```text
connection refused
level: error
/timeout|refused/i
message: /^timeout/
"field name": error
pl.col('status') >= 500
```

Literal search is case-insensitive. Field selectors use a colon followed by a
space; JSON-quote names with spaces or punctuation. Regex flags are `i`, `m`, `s`.
Use `\/var/log` to search for a literal leading slash. Search debounces for 300 ms;
clearing removes only its constraint, retaining the rest of the view.

In Enrichment (`e`), add:

```text
/request=(?P<request_id>\S+) status=(?P<status>\d+)/
```

Then add another stage:

```python
upper_id = pl.col('request_id').str.to_uppercase()
```

Both extracted fields and the new output remain available. Alt-A adds, Alt-E edits
and Alt-R removes the selected stage. Later stages can use earlier outputs. Invalid
changes preserve the whole last-good chain and filter membership. String
`replace`/`replace_all` are supported; arbitrary Polars operations are not yet
accepted. Python constructs advanced expressions; Rust Polars executes log batches.
Regex-only enrichment and literal/regex search do not require Python startup.

### Time, display and copy

Open Time with `t`: Alt-P uses capture time, Alt-E recognized event time, and Alt-U
the accepted `timestamp_utc` derived field. Alt-T prepares timestamp assistance;
that output name does not assume any particular input field name. Absolute windows
use `[start, end)` bounds; rolling presets are 5 minutes, 15 minutes and an hour.
Missing/invalid event timestamps do not silently fall back to capture time.

Left/Right scroll event text by eight terminal columns; `0` resets it. Metadata
columns stay fixed. Grouping changes presentation, not physical records or snapshot
membership. Merged views concatenate explicit source order, then record sequence;
they do not interleave by event time.

Drag visible text and press Ctrl-C to request a clipboard copy. With no selection,
Ctrl-C quits. Copy includes visible text only and requires OSC 52 support in the
terminal/multiplexer. Selection across dialog boundaries and redraw corruption
have been reported in preview032; fixes are in progress, not yet published.

## Preferences and data

Open settings with `,`. Themes: Terminal, Love Dark, Love Light, Dracula, Nord and
Gruvbox Dark. Preferences are stored in `$XDG_CONFIG_HOME/lvu/settings.toml`, falling
back to `~/.config/lvu/settings.toml`; see the
[complete example](docs/settings.example.toml).

New model configurations default to `codex/gpt-5.6-luna`; saved choices and explicit
environment overrides take precedence. `LVU_REDUCED_MOTION` makes animation static,
`LVU_NO_DELIGHT` disables accents, and `LVU_ASCII` enables ASCII rendering. Presence
activates these flags, even with an empty value or `0`.

Default durable data lives in `$XDG_DATA_HOME/lvu` (`~/.local/share/lvu`), separate
from disposable indexes in `$XDG_CACHE_HOME/lvu` (`~/.cache/lvu`). An existing legacy
`.lvu-captures` directory may be used with a notice; it is never moved automatically.
Cache settings take effect after restart. Memory defaults are 4 MiB of row payloads
and 256 MiB of membership payloads; derived indexes default to 256 MiB per source
and 5 GiB globally. These are managed-data budgets, not a process-wide RSS limit
or a quota on durable journals and investigations. Cleanup preserves durable data.

## Current limits

- Linux is the validated platform. Stdin needs a controlling terminal; EOF leaves
  the viewer open. Supported redirection includes pipes, regular files and `/dev/null`.
- Gzip files are static archives detected by magic bytes. Unchanged reopen avoids
  duplicates; changed/appended archives need a fresh capture identity. Gzip stdin
  and live-growing compressed archives are unsupported.
- Display field projection is bounded; nested JSON expansion is not built.
- The live expression compiler supports a subset of Polars. Broader acceptance,
  including pending datetime constructor work, is tracked in TODO; size/row-count
  checks alone do not prove record alignment or batch-independent semantics.
- Command enrichment has a library implementation but no application integration.
  HTTP acquisition, automatic command restart, ownership-aware capture retention
  and automatic cache eviction remain unfinished.
- Cancellation is checked between bounded batches; it cannot preempt a running
  Polars operation or kernel read. Snapshot limits do not imply complete model
  inspection or an enforced model sample count.

## Development

```sh
mise run build:app
./target/debug/lvu-app app.log
mise run check:rust
mise run check:expr
mise run install:bridge
mise run check:bridge
mise run test:pty
mise run test:pty:real
```

Additional focused PTY tasks cover search, enrichment, stdin, gzip, themes/settings,
copy and merged views; inspect `mise tasks` for the available commands. Expression
and PTY tasks use locked Python environments through uv. Use the pinned mise tools
and lockfiles; Python/Rust Polars compatibility is part of the test contract.

Contributors and agents should read [AGENTS.md](AGENTS.md),
[architecture](docs/architecture.md), [contracts](docs/contracts.md) and
[work ledger](docs/work-ledger.md). Planned scope lives in the
[implementation plan](docs/implementation-plan.md), not the supported-feature list.
