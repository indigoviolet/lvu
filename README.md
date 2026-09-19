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
  over open sources. Derived views delete permanently after confirmation
  (All events only with its source); sources remove from the workspace after
  confirmation while their captured bytes, journals and bookmarks stay on
  disk for reconnect.
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
  your own local agent CLI. Automatic log setup opens raw data
  first, then builds a reversible Enhanced view from a bounded sample;
  everything else works offline (`uv` only for Polars expressions, `node`
  only for the bridge).

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
found and where. `lvu --version` reports the installed release; `lvu --help`
lists the command-line options.

## Sixty seconds

Open a file, a command or a pipe:

```sh
lvu app.log worker.log archived.log.gz
lvu --command 'docker logs -f api'
producer | lvu
```

Run `lvu` again in the same capture directory and it restores the sources of
your last session. Files continue from their durable cursor. Remembered commands
and HTTP endpoints remain visible but are not launched or contacted until you
explicitly restart them; command-line source arguments are explicit starts and
are added to the remembered set rather than replacing it. `--resume` selects
this behavior explicitly. `--fresh` starts with nothing acquired; it deletes
nothing, and every capture and saved view stays in the workspace.

`N` opens Sources: existing sources remain listed even when stopped or waiting
for an explicit launch; select one to read its complete status, restart it or
remove it from the workspace. Its Add source action opens the separate creation
dialog. `n` goes straight to that Add source dialog, whose Manual tab opens
files or commands, while Discover checks processes/open files, project files,
Docker services/containers and previously opened sources, naming every category
checked and which produced no results. Results are separated by source type and
sorted by newest established activity within each section. Sources known to be
unavailable for more than two weeks are collapsed behind an explicit Show row;
filtering still finds them, and `Alt-U` expands or collapses them. Docker
discovery keeps every individual container source and also offers one
Compose-service source that follows all replicas when its Compose configuration
is available locally; it uses the same implicit Docker environment routing as
the CLI unless a context was explicitly configured. Agent proposes reviewed
sources. With no source and nothing to resume, Add source opens automatically.
Nothing starts capturing until you choose it.

In the viewer:

| Key | Opens |
| --- | --- |
| `/` | Filter, on its Search tab: a word, `field: value`, or `/regex/`; the view narrows as you type. Alt-A switches to the Advanced tab: a Polars expression such as `pl.col('status') >= 500`. Both apply at once |
| `e` | Enrichment: steps that add columns, from `/(?P<name>…)/` captures, `name = pl…` expressions, or an external command |
| `t` | Time: an absolute or rolling window over capture time, event time or an extracted timestamp |
| `i` | Fields: the record's structure with types and sample values; one key pins, filters, colours, folds or correlates a field |
| `d` | Details: the selected record as a tree beside the log |
| `b` / `B` | Bookmark the selected record / open bookmarks and notes; `Open in All events` stays there, while `Inspect context` returns with `o` |
| `n` / `N` | Add source directly / Sources list for full health, restart and confirmed removal |
| `v` | Views: new, cloned, merged and deleted views over open sources (Alt-E selects Delete; Enter arms, Enter confirms) |
| `Delete` | Remove the selected source from the workspace after confirmation; captured data stays on disk |
| `r` | Recipes: save, apply, export and review a view's setup |
| `z` / `m` | Grouping: Run, Filter or Off using enrichment columns |
| `f` | Follow the tail; `g` and `G` jump to the ends; `[` and `]` switch views |
| `?` | Help |
| `Ctrl-P` | The command palette: every operation, searchable, with its key |
| `,` | Settings, including the Automatic log setup policy (On new source by default) |

Escape closes a dialog. `q` quits from the base screen.

## What you can rely on

- **Captured bytes are never changed.** Parsing, filtering, enrichment,
  colouring and cleanup work on copies. The raw line is always one key away
  in Details, and a record keeps its identity across everything you do.
- **All events is the source, unfiltered.** Every source has a permanent All
  events view that cannot be filtered in place. Editing it forks a new view,
  so there is always a way back to everything that was captured. It cannot
  be deleted on its own; removing its source deletes it with everything else
  the source owns, and only after confirmation.
- **Deletion removes membership, never captures.** Deleting a view removes
  its workspace row only after the store acknowledges it; queued saves can
  never resurrect it. Removing a source stops its capture, drops it from
  session restore and deletes its owned views only after acknowledgement,
  refusing actionably while surviving views or unions still depend on it.
  Journals, capture directories, source bookmarks, recipes and proof data
  stay byte-identical.
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

Automatic log setup runs for each newly opened source by default, after its raw
All events view is already usable. Set `Automatic log setup > Run` to `Disabled`
to opt out. A successful bounded proposal creates an
ordinary Enhanced view containing only native enrichments, pins, exact-value
colour rules, display roles and Run/Filter grouping. It cannot add a filter,
command, source or time window. `Current log > Analyze again` runs the same
operation explicitly; `Current log > Setup status` opens a full inspector for
that log before offering another analysis. It names the log, lifecycle state,
Paseo session (or that one is still being created), and diagnostic; its
`Analyze again` button is an explicit retry. `Current view > Revert automatic setup` removes an
unchanged generated view while preserving its source and captured bytes. Once
you edit that setup manually, lvu refuses the automatic revert so it cannot
erase your work. While analysis runs, the footer names its current stage without
blocking raw browsing. Its `Paseo session: <session>` result identifies a retained
`lvu automatic log setup` conversation in Paseo; these conversations are not
archived when the one-shot proposal completes.

Two assistance failures look similar but have different remedies. An owned-route
busy report names the exact `bridge.lock` under the capture's `assistance`
directory: close all lvu windows using that capture root first; only when none
remain, verify no lvu or bridge process still owns it, then remove only that
exact lock file — never the assistance root or capture data. A daemon-unreachable
report means the machine/container running lvu has no Paseo daemon at
`LVU_PASEO_URL` (default `ws://127.0.0.1:6767/ws`): establish topology
(Desktop-managed, standalone, or Docker) and check `paseo daemon status --json`.
A remotely connected client does not imply localhost has a daemon.

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
