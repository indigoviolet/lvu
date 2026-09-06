# lvu — Love You Log Time

lvu is a terminal log viewer written in Rust, with Polars for filtering and
enrichment. It reads files, process output and streams, and supports multiple
saved views over the same captured data.

- **Sources:** plain files, gzip archives, command stdout/stderr and stdin.
  Capture is shared across views, with background indexing, paged display and
  incremental processing of new records.
- **Polars expressions:** write filters and transformations using Python Polars
  syntax; Rust executes the expressions in batches. Literal and regex operations
  do not require Python.
- **Filtering and enrichment:** search raw text or named fields, use regexes,
  extract named captures and add derived columns in successive steps.
- **Time and display:** live follow, history, absolute and rolling time windows,
  pinned columns, severity colors, color by value and multiline grouping.
- **Source discovery:** candidates from Docker, Linux processes, project files
  and remembered sources, with explicit selection before capture starts.
- **Preserved data:** filters and enrichments leave the original bytes intact.
  Invalid edits retain the last accepted view.
- **Saved work:** independent views, bookmarks, notes, versioned recipes and
  restoration of settings and unfinished drafts.
- **Optional 🧠 assistance:** use your coding-agent provider to suggest filters,
  enrichments and timestamp extraction, or run a resumable investigation.

## Quick start

Homebrew and mise package installation are planned. For now, build from source
with [mise](https://mise.jdx.dev/) installed:

```sh
git clone https://github.com/indigoviolet/lvu.git
cd lvu
mise trust
mise install
mise run build:app
./target/debug/lvu-app app.log
```

Open several files, read a compressed archive, or capture a command or pipe:

```sh
./target/debug/lvu-app app.log worker.log archived.log.gz
./target/debug/lvu-app --command 'docker logs -f api'
producer | ./target/debug/lvu-app
```

Run without a source to enter the workspace and discover logs. In the app,
`?` opens Help and Ctrl-P opens the searchable command palette. These list the
available actions and controls. Use `--help` for command-line options.

For optional 🧠 features, configure a supported local coding-agent provider and
build the adapter from the checkout:

```sh
mise run install:bridge
mise run build:bridge
```

Choose your model in the app's settings. Provider setup and authentication use
your existing account; they are separate from installing lvu.

## Features

### Capture and source discovery

Read plain files, gzip archives, command stdout/stderr and piped input. Follow
files as they grow, retain rotation/truncation boundaries, and resume file capture
without duplicating acknowledged records. Original bytes remain preserved when
text is parsed, filtered or enriched.

Open several views over the same capture without starting the source again.
Combine already-open sources into one view, or keep separate views for different
questions. Stop and restart sources explicitly; restoring a workspace doesn't
silently launch remembered commands.

Source discovery brings Docker, process, project and remembered candidates into
one place. Selecting a candidate doesn't automatically start it.

### Filtering

Use a word, a field selector, a regex or a Polars Boolean expression:

```text
connection refused
level: error
/timeout|refused/i
message: /^timeout/
pl.col('status') >= 500
```

Search updates as you type. Combine it with additional filters and time bounds.
An invalid expression leaves the last accepted view usable.

### Enrichment

Named regex captures become columns:

```text
/request=(?P<request_id>\S+) status=(?P<status>\d+)/
```

Add a later step using those fields:

```python
upper_id = pl.col('request_id').str.to_uppercase()
```

Earlier successful steps stay in place. Edit or remove steps, inspect the raw
input alongside derived values, and reuse the resulting workflow as a recipe.
Python expression syntax gives you Polars transformations while Rust handles
execution; this is not arbitrary per-line Python scripting.

For external transformations, a reviewed command step can follow the Polars
stages. Inspect its fixed input before running it and read its results in Details.
Saving a command definition or reopening a workspace never executes it.

### Time and display

Filter by capture time, recognized event time or an extracted UTC timestamp.
Choose an absolute range or a rolling window. Ask 🧠 to suggest timestamp
extraction when the format is unfamiliar, then review the result.

Pin fields beside the log, distinguish severity and values with color, and group
multiline messages or stack traces without changing the underlying records.
Browse neighboring raw records when a filtered result needs context. Horizontal
scrolling, bookmarks, notes and visible-text selection help with long events.

Themes include Terminal, Love Dark, Love Light, Dracula, Nord and Gruvbox Dark.
The interface includes a pixel-art startup screen and heartbeat activity indicator,
with reduced-motion and ASCII options.

### Views and recipes

Named views retain their own filters, enrichments, time bounds and presentation.
Recipes let you reuse that setup, export it, review earlier revisions and adapt
it to another source. Workspace state includes both accepted settings and
unfinished drafts.

### 🧠 assistance

Request a filter or enrichment in plain language, identify a timestamp format,
or start a separate, resumable investigation over a fixed snapshot. Suggested
changes are reviewed and validated before they affect your view.

Capture and query execution stay local. If you choose a hosted model, context read
by its agent is subject to that provider's data handling. Assistance is optional;
lvu remains useful without it.

## Platforms and installation

**Linux is currently validated.** macOS and Windows support need platform testing.
Use a UTF-8 terminal for the full interface; clipboard copying depends on terminal
support for OSC 52.

Homebrew and mise release packages are on the [roadmap](TODO.md). Until those are
available, the source checkout is needed for the expression helper and 🧠 adapter.
See the [installation plan](docs/distribution.md) for packaging status.

## Current limitations

- Gzip input is a static archive, not a live compressed stream.
- Merged views preserve source order; they do not interleave events by timestamp.
- Expressions must operate independently on each record. Cross-record operations
  such as sorting, aggregation and window functions aren't live enrichments.
- One external command step is supported. Its outputs are shown in Details, not
  yet available as inputs to later Polars filters or recipes.
- Nested JSON expansion, HTTP sources and automatic command restart are not yet
  supported.
- Cache budgets limit managed data, not total process memory or durable capture
  storage. Capture files and investigation snapshots can continue to use disk.

## Planned work

Planned and ongoing work includes Homebrew and mise packages, faster 🧠 requests,
revised Time controls and field correlation across sources. See the [task list](TODO.md) for open
work and [preview notes](docs/previews.md) for version-specific compatibility.

Build and test instructions are in [development notes](docs/development.md).
The [architecture document](docs/architecture.md) describes the implementation.
