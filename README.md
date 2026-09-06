# lvu — Love You Log Time

**Follow the logs. Find the signal. Keep what you learn.**

lvu is a terminal workspace for exploring logs from files, running processes and
containers. Start with raw text, extract structure as you go, and keep useful
filters and views for the next investigation.

- **Capture once, explore many ways.** Views share a durable capture. Background
  indexing, paged display and incremental query processing keep new arrivals
  moving without loading the whole log into the terminal's row cache.
- **Rust underneath, Polars at the controls.** Filter and transform with Python
  Polars expressions; Rust executes them in batches. Plain text and regex work
  without starting Python.
- **Turn text into fields, one step at a time.** Search raw text or named fields,
  extract regex captures, then build further enrichments on earlier results.
- **Find the relevant moment.** Follow live output, browse history, narrow a time
  window, pin useful columns and color values so patterns are easier to spot.
- **Discover sources nearby.** Find candidate logs from Docker, Linux processes,
  project files and remembered sources, then choose what to open.
- **Experiment without losing the original.** Raw logs stay intact. Invalid
  filters or enrichments leave the last working view in place.
- **Keep the workflow, not just the search.** Save independent views, bookmarks,
  notes and versioned recipes. Return to accepted filters and unfinished drafts
  after restart.
- **Bring your own 🧠.** Use a supported coding-agent provider to suggest filters,
  enrichments and timestamp extraction, or start a deeper investigation. Review
  proposals before applying them; everyday log browsing needs no model connection.

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
**`?` opens Help** and **Ctrl-P opens the searchable command palette**. Actions and
options belong there—you don't need to memorize a shortcut sheet to get started.
Use `--help` for command-line options.

For optional 🧠 features, configure a supported local coding-agent provider and
build the adapter from the checkout:

```sh
mise run install:bridge
mise run build:bridge
```

Choose your model in the app's settings. Provider setup and authentication use
your existing account; they are separate from installing lvu.

## Explore your logs

### Sources that keep their history

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

### Search simply, then get precise

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

### Enrich without starting over

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

### Time, context and visual cues

Filter by capture time, recognized event time or an extracted UTC timestamp.
Choose an absolute range or a rolling window. Ask 🧠 to suggest timestamp
extraction when the format is unfamiliar, then review the result.

Pin fields beside the log, distinguish severity and values with color, and group
multiline messages or stack traces without changing the underlying records.
Browse neighboring raw records when a filtered result needs context. Horizontal
scrolling, bookmarks, notes and visible-text selection help with long events.

Choose Terminal, Love Dark, Love Light, Dracula, Nord or Gruvbox Dark. An animated
pixel-art title and heartbeat add a little character; reduced-motion and ASCII
options are available.

### Save the useful parts

Named views retain their own filters, enrichments, time bounds and presentation.
Recipes let you reuse that setup, export it, review earlier revisions and adapt
it to another source. Workspace state includes both accepted settings and
unfinished drafts.

### Bring your own 🧠

Ask for a filter or enrichment in plain language, get help identifying a timestamp,
or jump into a separate, resumable investigation over a fixed snapshot. Suggested
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

## A few current boundaries

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

Next up: easier installation, faster 🧠 requests, a clearer Time form and field
correlation across sources. The [task list](TODO.md) tracks what's still open;
[preview notes](docs/previews.md) cover build-specific changes and compatibility.

Looking under the hood? See [development notes](docs/development.md) and
[architecture](docs/architecture.md).
