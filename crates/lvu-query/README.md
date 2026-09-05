# lvu-query

Native, bounded-batch query primitives. `_lvu_*` columns are protected metadata;
`raw` is the public display alias, while `_lvu_raw_bytes` remains authoritative.
The tolerant adapter projects homogeneous JSON booleans/numbers as native nullable
columns and parses bounded quoted logfmt, retaining original bytes independently.
`SchemaContext` carries known nullable columns across batches. Conflicting values
become null in the established canonical column while a protected per-row type
provenance column and bounded diagnostic keep the conflict distinguishable.

Nested JSON values currently remain JSON text. Evolving nested schemas, escaped
logfmt quoting, multiline logical-event assembly, and cross-batch type promotion
remain integration work; none of those limitations discard physical records.
Chunk position, acquisition identity, delimiter, and stable source/sequence IDs
are retained so a later framing layer does not reinterpret long-line fragments as
lost input.

The compiler host bounds one in-flight request, request/response lines, retained
stderr, and wall time. Cancellation and timeouts kill and wait for the owned
child, then a later request starts a fresh helper. These are application limits,
not an OS sandbox or CPU/address-space quota; Python definitions are trusted local
code. Native collection itself is not interruptible here, so query cancellation
is observed between explicitly bounded DataFrame batches. Query batches flow into
an explicitly bounded page sink; generation state retains only commit metadata.

`TextSearch` is a Rust-constructed native expression over `_lvu_raw`: empty text is
unconstrained, other text uses Unicode lowercase plus literal substring matching,
and an optional advanced validated Polars filter is combined with it using AND.

A leading-slash enrichment such as
`/request_id=(?P<request_id>\S+).*status=(?P<status>\d+)/` is parsed and compiled
entirely in Rust. Each named capture becomes an ordered native
`col("raw").str.extract(pattern, capture_index)` stage and unmatched captures are
null. `\/` represents a literal slash delimiter; optional trailing `i`, `m`, and
`s` flags are supported. Patterns, compiled regex size/nesting, stage identities,
definition count, output count, and output names are bounded. Anonymous-only
patterns, duplicate/protected outputs, invalid flags, and invalid regexes fail the
candidate. `parse_regex_enrichment` exposes capture names/indexes and equivalent
Polars source for UI/recipe presentation; execution does not invoke Python.

`EnrichmentDefinition` and `compile_enrichment_chain` model an ordered chain with
stable stage identities. The entire candidate chain validates before publication;
later definitions execute after earlier outputs and may depend on them. The legacy
single enrichment string maps to one definition during persistence migration.
