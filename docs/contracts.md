# Shared contracts, revision 1

These contracts fix semantic boundaries before parallel implementation. Concrete
Rust signatures can evolve with primary-agent review; do not duplicate competing
models. JSON uses snake_case names. Persisted records include schema_version.

## Identities and records

SourceId, ViewId, RecipeId, InvestigationId: opaque UUID strings. RecordId is the
pair (source_id, monotonic u64 sequence), unique across capture restart. SegmentId
identifies immutable journal extent; offsets are byte offsets. StreamKind is
stdout, stderr, file, or http. Ingest timestamp is UTC integer nanoseconds when
available; source ordering always uses sequence, never a clock alone.

RawRecord stores record_id, capture timestamp, stream, raw bytes, acquisition
instance/boundary metadata. Original delimiters/invalid UTF-8 must be recoverable.
Display text is a lossy/escaped projection; it is not authoritative storage.
LogicalEvent stores ordered constituent RecordIds and optional event timestamp.
Multiline grouping cannot delete the underlying physical records.

Derived fields live separately from protected identity/raw/capture metadata.
Each stage has revision, dependency revisions, schema version, and engine version.
DerivedState: pending, ready, unmatched, or error with bounded diagnostic text.
Missing/null/type-error are represented explicitly when materially different.

## Sources and views

SourceDefinition: schema_version, id, name, kind, acquisition configuration,
stable identity hints, optional retention policy. Kinds: file(path, follow),
command(shell text or executable/args, cwd, environment overrides, restart policy),
http(url, framing, reconnect policy). Credentials are not printed in diagnostics.
Status transitions: starting, running, paused/backpressured, exited/disconnected,
storage_blocked, error, stopped. Capturing and UI follow mode are independent.

ViewDefinition: id, name, source_ids, recipe stage revisions, filter definition,
time window, presentation (pins/color rules), navigation settings. Applied
revision and editor draft are distinct. A new query generation cancels or fences
older results. Matching results retain stable RecordIds/logical EventIds.

## Expression helper: JSON Lines request/response

Request: {schema_version:1, request_id, operation:"compile", kind:"filter"|
"enrichment"|"color", expression:string}. Response echoes request_id and returns
either {ok:true, expression_json:string, python_polars_version:string,
compatibility_id:string} or {ok:false, error:{code,message}}. Exact metadata may
expand after compatibility proof. stdout is protocol only; diagnostics use stderr.
Request limits, wall timeouts, cancellation, and helper restart are host duties.
Stored Python text is authoritative; expression_json is a regenerable cache.

Native row-local expressions only in initial live mode. Reject Python callbacks,
plugins, aggregate/window/sort/filter/head/unique/explode/gather and other
cardinality/order-changing constructs unless explicitly proven safe. Shape checks
alone do not establish row alignment. Protect metadata column names. Python
evaluation is executable local code; do not claim that restricted globals are a
security sandbox. Never evaluate definitions silently just because they were
found in discovered files. User-applied definitions run as user code with limits.

## Command enrichment: JSON Lines

Managed batches use tagged JSON objects: `batch_begin` with session, revision,
event_count; `event` with session, revision, event_id, raw, fields; then `batch_end`
with session and revision. EventId is {source_id: UUID string, sequence: u64}.
Outputs are tagged `event` objects carrying session, revision, event_id and fields,
followed by matching `batch_complete`. Commands need a wrapper for this protocol.
Reject duplicate/unknown IDs, stale session/revision, protected-field writes,
malformed/oversized output and missing/premature completion. Silence is not a
completion boundary. Successful independent event fields remain available after
a batch failure; consumers must also inspect batch diagnostics.

Bound outstanding inputs and output bytes; timeout unreturned IDs without
discarding raw input. stderr is diagnostic, not event data. Persistent children
must be cancelled/reaped. Delivery preserves input order; responses may arrive in
another order and are joined by stable ID. A caller-owned capacity-bounded
AttemptLedger survives child resets and refuses work at capacity without eviction.
Its compatibility path is in-memory only. `run_batch_with_store` reserves attempts
before writer delivery and persists the final protocol-validated outcome through
an externally scoped AttemptStore. Reservation tokens own exactly one batch's IDs;
prior results in mixed batches must not be overwritten. Scope identifies the
command and preceding definitions, never a changing live-data revision.

Serialization/spawn preflight errors consume no attempts. Once reservation is
attempted, an ambiguous acknowledgement fails closed with no delivery. Reserved
IDs remain attempted even if cancellation prevents delivery or final persistence
fails. Cancellation/deadline is rechecked after reservation; synchronous stores
must enforce their own bounded transaction/lock waits. Reserved-without-result
means attempted/result unavailable, not permission to retry.

Post-preview033 source adds SQLite workspace schema v4 for atomic reservations,
owned immutable completion and bounded ordered lookups. Ready fields preserve JSON
types and optional diagnostic evidence; raw bytes remain in capture. Result reads
and completion batches have a 1 MiB encoded payload budget. The source application
wires these to a reviewed optional terminal command stage with frozen native input
and read-only Details results; preview033 does not include it. Command results are
durable data, not automatically reproducible cache entries.

The application execution module uses a stricter publication policy
than the compatibility runner: a global batch diagnostic marks every newly reserved
ID Failed. Event replies without a valid completion frame cannot become publishable
by invoking Run again. A valid completed batch may retain independent Ready records
alongside event-level failures. A previously finalized Ready result is immutable.
Application metadata keeps the saved terminal command definition separate from
the reference to the last published result set; saving an edit does not replace
old results or launch a command. Result references bind their owning view and
exact record IDs. Combined actual-app acceptance includes saving without execution,
review cancellation, new-only delivery, restart, malformed-output rollback and
save-then-quit persistence. Result save admission is the commit boundary: freshness
and cancellation are checked before dispatch, then Saving results permits closing
without cancellation. Acknowledgement attaches the accepted publication even if
the dialog closed; save failure retains the prior reference. Restoring that
immutable reference is independent of later command-definition edits.

## Paseo bridge: JSON Lines request/response/events

Versioned requests carry request_id and method; responses echo request_id and
either result or structured error; asynchronous events carry session_id and kind.
Minimum methods: capabilities, start_session, send_prompt, cancel, resume_session,
request_proposal. Proposals carry kind (source/filter/enrichment/view), definition,
explanation, and data/definition revision used. Reject stale proposals at apply.
The bridge handles Paseo, not Polars execution or bulk log transport. Inspection
context is a local manifest path plus dataset paths. Provider-neutral API; one
working provider integration suffices. Preserve native permission interaction if
the SDK emits it; do not invent a provider login flow.

## Dataset/investigation manifest

schema_version, investigation_id, created_at, capture high-watermarks per source,
resolved absolute time range, exact ViewDefinition and recipe revisions, selected
event IDs, schema/field provenance, filtered Parquet paths, full-source dataset or
segment references, and ownership pins. Export metadata distinguishes complete,
pending, and failed enrichment. Materialize filtered data atomically; a manifest
must not claim completed files that are still being written. Refresh creates a
new immutable capture boundary. No mandatory generated Python programs.

## Ownership and cache classes

Disposable: rendered rows, matches, native deterministic enrichment columns,
indexes, serialized expressions. Durable: raw captures, command enrichment
results, configurations, investigation-owned data. Cache keys include segment,
recipe/schema/engine revisions. Eviction requires zero active readers/pins;
cleanup cannot follow symlinks outside managed storage. Refcounts/ownership are
transactional and recoverable after restart. Memory budgets count bytes, not just
entry counts. Query working memory is reported separately from managed caches.

## Default search interaction (user clarification)

The default view input is literal plain-text substring search over displayed/raw
message text, case-insensitive by default. Punctuation has no regex or Polars
meaning. Empty search imposes no constraint. Advanced Polars filtering is an
optional separate constraint and combines with text search using AND. Each view
remembers independent search and advanced-filter draft/applied state. The UI must
make active constraints visible and clearing either must preserve the other.
Live search uses bounded asynchronous query work and generation fencing; no
Python helper is needed for literal search. Advanced definition assistance remains
available when the user opts into it.

## Inline recipe adaptation (2026-09-06)

A typed View proposal may include optional `enrichments`, an ordered array of
`{id, source}` definitions. Limits: 32 stages, 128-byte IDs, 16 KiB editable
sources; duplicate IDs are rejected by the host. Each source uses the existing
named Polars-expression or named-capture regex syntax. Omission retains the
reviewed recipe chain; an empty array explicitly proposes clearing it. Unresolved
`recipe_stage_revisions` remain unsupported. The advanced filter and complete
chain are reviewed and applied through one native recipe transaction. Other
search, time, grouping and presentation settings are retained. The JSON schema,
host validation, view/revision fences and native semantic validation all apply.
The complete request context is capped at 128 KiB before snapshot work begins.

## Predicate colour rules (2026-09-07)

A view carries an ordered list of at most 16 `{predicate, color}` rules. The
predicate is written in the search box's own language — literal, `field: value`,
`/regex/flags`, or a `pl.…` expression — and is compiled and evaluated by the
query engine through the same `TextSearch` the filter uses; the terminal never
decides whether a rule matched. The first rule whose predicate matches a row
wins, so the order the user gave is the precedence, and the engine reports the
winner as the `color_rule` presentation detail carrying its 1-based position.

Rules are presentation, not definition:

* applying them submits one query and never narrows the view, so `matched` is
  unchanged and the last applied rows stay on screen while it settles;
* a canonical view is repainted in place rather than forked, because its
  *definition* is fixed and its presentation never is;
* a rule that will not compile is skipped with the rest still evaluated, and the
  view keeps rendering;
* the view-adapter staleness check compares definitions and ignores rules, so an
  unacknowledged repaint cannot make a later filter look stale.

The colour is a token from a closed set (`red`, `orange`, `yellow`, `green`,
`cyan`, `blue`, `purple`, `magenta`), not an RGB triple, so the theme re-runs
the same contrast check the hashed identity colours use — including snapping
into the xterm cube first on a 256-colour terminal.

Persistence is additive: `presentation_json.color_rules`, `serde(default)` like
every other presentation field, with **no `DB_SCHEMA_VERSION` bump**. An older
binary reading a newer row ignores the key; a newer binary reading an older row
gets an empty list. The cost of that choice, stated rather than hidden: a *save*
by an older binary drops the rules. The alternative — bumping the schema so old
binaries refuse the workspace outright — trades silent loss of a presentation
setting for a hard refusal on every downgrade.

Span highlighting is a separate, terminal-side concern and decides nothing about
membership: it re-locates a search or rule pattern inside the line already being
drawn. Spans are computed on the rendered `String`, which is already
`from_utf8_lossy` of the captured bytes, so a replacement character cannot shift
a highlight — searching the original bytes and reporting offsets into them
would, because one invalid byte becomes three. Only pattern forms are located; a
`field: value` or `pl.…` predicate names a column, not a run of characters, and
underlines nothing.
