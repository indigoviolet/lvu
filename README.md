![lvu startup splash: animated terminal rendering of a red heart above "LOVE YOU LOG TIME" lettering](assets/startup/love-you-log-time/120x40-sharp/ansi-preview.gif)

# lvu

lvu is a terminal log viewer. It captures files, command output and stdin
into a durable local store, then lets you search, filter, enrich and bookmark
what it captured without ever changing the captured bytes. Filters are literal
words, `/regex/` or Polars expressions; enrichment adds derived columns in
steps, including named regex captures and reviewed external commands. Several
views can look at the same capture from different angles, and a view's setup
can be saved as a recipe. Optional assistance can draft filters, enrichments
and timestamp rules through a local coding-agent provider, but everything
except that works offline.

## Features

- **Lossless durable capture.** Files, command output, and stdin land in a
  local store with byte-exact records and stable identities; files resume
  from a durable cursor, so a restart repeats nothing.
- **Many sources, many views.** Open several sources at once; each keeps a
  permanent unfiltered All events view, and views can be cloned or merged
  over open sources.
- **Live unions with shared keys.** Combine existing views into one live
  union, matching inputs on an accepted enrichment column pinned at creation.
- **Filtering that meets you halfway.** Literal words, `field: value`,
  `/regex/`, and Polars expressions, with search and advanced constraints
  combining.
- **Native enrichment, reviewed commands.** Add derived columns from named
  regex captures or Polars expressions evaluated by the Rust engine;
  external commands run only after explicit review, with durable results.
- **Presentation that never rewrites data.** Time windows over capture,
  event, or extracted time shown in your configured zone; Run/Filter
  grouping, folding, and ordered colour rules — all display-only, with
  last-good rollback on invalid drafts.
- **Bookmarks, recipes, snapshots.** Bookmark records with notes, save a
  view's setup as a revisable recipe, and export fixed snapshots for
  investigation.
- **Assistance optional, offline core.** Plain-language filter, enrichment,
  and timestamp drafting plus resumable snapshot investigations through
  your own local agent CLI; everything else works offline (`uv` only for
  Polars expressions, `node` only for the bridge).

## Install

Homebrew:

```sh
brew install indigoviolet/tap/lvu
brew install uv node   # optional: Polars expressions, assistance
```

mise, globally or in a project's `mise.toml`:

```sh
mise use -g github:indigoviolet/lvu
```

Supported platforms are Linux x86_64/arm64 and Apple-silicon macOS. Linux x86_64
has full release acceptance; Apple-silicon macOS has automated installation and
terminal-cleanup checks, with human terminal acceptance still outstanding.
Intel Macs and Windows are unsupported.

Build from source with [mise](https://mise.jdx.dev/) if there is no package
for your platform:

```sh
git clone https://github.com/indigoviolet/lvu.git
cd lvu
mise trust && mise install
mise run build:app          # target/debug/lvu-app is the application
```

`lvu --resources` reports which expression helper and assistance bridge it
found and where. `lvu --help` lists the command-line options.

## Sixty seconds

Open a file, a command or a pipe:

```sh
lvu app.log worker.log archived.log.gz
lvu --command 'docker logs -f api'
producer | lvu
```

Run `lvu` again in the same capture directory and it re-acquires the sources of
your last session: files continue from where their capture stopped, and command
sources run again with the directory and environment they were recorded with.
`--resume` says so explicitly. File and command arguments are added to that set
rather than replacing it. `--fresh` starts with nothing acquired; it deletes
nothing, and every capture and saved view stays in the workspace.

With no source and nothing to resume, lvu opens the Add source dialog: Tab
completes paths, Ctrl-D discovers Docker containers, processes and project
files. Nothing starts capturing until you choose it.

In the viewer:

| Key | Opens |
| --- | --- |
| `/` | Filter, on its Search tab: a word, `field: value`, or `/regex/`; the view narrows as you type. Alt-A switches to the Advanced tab: a Polars expression such as `pl.col('status') >= 500`. Both apply at once |
| `e` | Enrichment: steps that add columns, from `/(?P<name>…)/` captures, `name = pl…` expressions, or an external command |
| `t` | Time: an absolute or rolling window over capture time, event time or an extracted timestamp |
| `i` | Fields: the record's structure with types and sample values; one key pins, filters, colours, folds or correlates a field |
| `d` | Details: the selected record as a tree beside the log |
| `b` / `B` | Bookmark the selected record / the bookmarks list with notes |
| `v` | Views: new, cloned and merged views over open sources |
| `r` | Recipes: save, apply, export and review a view's setup |
| `z` / `m` | Grouping: Run, Filter or Off using enrichment columns |
| `f` | Follow the tail; `g` and `G` jump to the ends; `[` and `]` switch views |
| `?` | Help |
| `Ctrl-P` | The command palette: every operation, searchable, with its key |

Escape closes a dialog. `q` quits from the base screen.

## What you can rely on

- **Captured bytes are never changed.** Parsing, filtering, enrichment,
  colouring and cleanup work on copies. The raw line is always one key away
  in Details, and a record keeps its identity across everything you do.
- **All events is the source, unfiltered.** Every source has a permanent All
  events view that cannot be filtered in place. Editing it forks a new view,
  so there is always a way back to everything that was captured.
- **A view you break stays usable.** An invalid search, filter or step
  leaves the last accepted view on screen and tells you what was wrong.
- **Folding and grouping are presentation.** Collapsing repeated lines or
  joining a stack trace changes what the pane shows, never what was
  captured, and never the counts a filter reports.
- **A source resumes without repeating.** Stopping and restarting a file or
  command continues from the durable cursor. A restored workspace never
  launches a remembered command on its own.
- **A command step runs only when you say so.** Saving, reopening, cloning
  or applying a recipe never executes it. A run shows its exact input first.
- **Memory and disk are bounded** by the caches and retention you set in
  Storage (`S`) and Settings (`,`). Capture files stay until you clean them.

## What assistance needs

Assistance drafts filters, enrichments and timestamp rules from a request in
plain language (`A`, Ask 🧠), and runs a resumable investigation over a fixed
snapshot (`I`, Investigation 🧠). It uses a local bridge to a coding-agent
provider you already have. It needs:

- `node` on `PATH` and the bridge that ships with lvu (`lvu --resources`
  shows whether it was found);
- an installed and authenticated agent CLI; choose the provider and model in
  Settings. lvu does not install or authenticate providers.

Suggestions are proposals: you review and validate them before they touch a
view. Capture and query execution stay local; text a hosted model reads is
subject to that provider's terms.

Without assistance, everything else works. Polars expressions need `uv` on
`PATH`, which provisions the pinned interpreter and Polars the first time
they are used. Literal and `/regex/` search, regex enrichment, views,
bookmarks, recipes and export need nothing beyond the binary.

## How it's built

Rust throughout: capture and indexing in `lvu-ingest` and `lvu-live`, queries
in `lvu-query` over Polars, the terminal in `lvu` (ratatui) and the
application shell in `lvu-app`. Python only constructs Polars expressions
when a definition changes; the bridge is TypeScript.

The design documents are indexed in [`docs/README.md`](docs/README.md).
Start with:

- [`docs/architecture.md`](docs/architecture.md) — the implementation map.
- [`docs/dialog-system.md`](docs/dialog-system.md) — how every dialog
  looks and behaves, and the rules they share.
- [`docs/component-model.md`](docs/component-model.md) — how the terminal
  shell hosts dialogs as components.
- [`docs/distribution.md`](docs/distribution.md) — packaging, resource
  resolution and the release sequence.
- [`docs/development.md`](docs/development.md) — building and testing.

Known gaps are listed in [`TODO.md`](TODO.md); what each version shipped is in [`CHANGELOG.md`](CHANGELOG.md).
