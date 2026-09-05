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

Input: {event_id:string, raw:string, fields:object}; output:
{event_id:string, fields:object}. One object per input; reject duplicate/unknown
IDs, protected-field writes, malformed/oversized output. Bound outstanding inputs
and output bytes; timeout unreturned IDs without discarding raw input. stderr is
diagnostic, not event data. Persistent child processes must be cancelled/reaped.
Results are retained derived captures by default, not automatically reproducible
cache entries. No automatic rerun after eviction or restart.

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
