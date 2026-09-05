# Polars expression JSON compatibility proof

## Result and pins

This spike proves the `lvu` contract using Python Polars **1.44.1** and Rust
Polars **0.55.2**, with exact direct pins and committed `uv.lock`/`Cargo.lock`.
They deliberately have different public version schemes. The compatibility key
is `polars-py-1.44.1-rs-0.55.2-expr-json-v1`; a different key is rejected.
At request time the helper verifies the imported `pl.__version__` is exactly
1.44.1 before returning that key. Every helper response carries
`schema_version: 1`.

Provenance checked on 2026-09-05:

- Polars documents `Expr.meta.serialize(format="json")` and
  `Expr.deserialize(..., format="json")`, while warning that serialization is
  not stable across Polars versions:
  <https://docs.pola.rs/api/python/stable/reference/expressions/api/polars.Expr.meta.serialize.html>
- The official `py-1.44.1` tag pins Python package 1.44.1 and has Rust workspace
  version 0.55.1:
  <https://github.com/pola-rs/polars/tree/py-1.44.1>
- Rust crate 0.55.2 was the latest stable crates.io release, and its official
  `Expr` source derives serde only behind the `serde` feature. The `serde-lazy`
  Polars facade feature enables that path:
  <https://docs.rs/polars-plan/0.55.2/polars_plan/dsl/enum.Expr.html>

The adjacent Rust patch release is intentional: compatibility is established by
the executable differential proof, not inferred from version-number equality.
Any pin or enabled-feature change must mint a new compatibility key and rerun it.

## Supported live subset

The helper accepts one native `pl.Expr` composed from columns/literals, arithmetic,
comparisons/boolean operators, conditionals, null checks/row-local value fill,
aliases, regex extraction and `str.contains` (regex or literal), casts (including
`strict=False`), struct-field access, and date/time parsing with an explicit
format. Format inference is rejected because inference can differ by batch. It
rejects unknown names and calls, forward/backward fill strategies, callbacks,
plugins/UDFs, selectors, aggregates, windows, sorting, gather, filter, explode,
slicing, reverse/shift, and other order/cardinality/global operations.

Identity, raw, and capture metadata are protected from enrichment writes, not
from reads. Filters may read `_lvu_raw` and the other reserved columns. For the
production schema, this proof recommends exposing `raw` as the documented public
predicate column while retaining `_lvu_raw` as the protected internal name; the
primary query/schema integration must choose and consistently map that boundary.

Python AST allowlisting runs before trusted local evaluation. Python then
round-trips the emitted JSON to detect corrupt output, but deliberately does not
scan arbitrary serialized strings or object keys: field names and literal payloads
such as `Reverse`, `Shift`, `Agg`, and `Sort` are ordinary data. After
deserialization, Rust recursively
matches a positive allowlist of `Expr`, `FunctionExpr`, string, boolean, and struct
variants and fails closed on every other variant. A modified cache therefore
cannot add `Reverse`, `Shift`, fill strategies, aggregates, or unknown functions.
This is a
support boundary, **not a security sandbox**; applying Python definitions executes
trusted local user code under host-owned process/time/resource limits.

The ten fixture expressions are shown batch-invariant by comparing whole-fixture
evaluation with independent one-row partition evaluation; this includes a
partition containing only the malformed timestamp. This evidence is intentionally
limited to those expressions rather than a claim about every possible allowed
composition. Row-locality policy comes from the positive allowlists, not equal row
counts. The Rust runner also checks fixture height as a last-line invariant.

Case conversion (`str.to_uppercase` and `str.to_lowercase`) is supported in both
the Python compiler and native validator. Its fixtures cover null propagation and
Unicode expansion (`Straße` → `STRASSE`). Unsupported-operation diagnostics name
lvu's current support boundary rather than claiming every omitted method needs
neighboring records.

## What the executable proof does

`differential.py` compiles the authoring strings once, independently evaluates
the deserialized Expr in Python, then sends only expression JSON and compatibility
metadata to a separate native runner process. The Rust fixture is constructed in
the Rust binary; no records or batches cross Python. The test compares exact
values, null positions, and dtypes for arithmetic, boolean logic, regex capture,
tolerant casts, nested struct access, timestamp parsing, and literal string
containment over raw text. It also exercises malformed and deeply nested requests,
wrong compatibility metadata, tampered aggregate/reverse/shift/fill-strategy and
format-inferred datetime ASTs,
oversized inner expressions, and oversized complete JSONL envelopes with recovery.

The final `PASS independent native execution` line is emitted only after all 56
Rust-produced whole/partitioned cells match. The printed Rust PID demonstrates that evaluation ran
in the separate executable; `ldd target/debug/lvu-polars-interop | grep -i python`
also returns no Python runtime dependency on Linux.

## Reproduction and proposed root mise tasks

Run from the repository root (the environment variables keep the only native
build small):

```sh
mise exec python@3.12.14 uv@0.12.10 -- uv sync --project python --locked
mise exec python@3.12.14 uv@0.12.10 -- uv run --project python pytest -q python/tests
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 RUSTFLAGS='-C debuginfo=0' \
  mise exec rust@1.98.1 -- cargo test --manifest-path spikes/polars-interop/Cargo.toml --locked
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 RUSTFLAGS='-C debuginfo=0' \
  mise exec rust@1.98.1 -- cargo build --manifest-path spikes/polars-interop/Cargo.toml --locked
mise exec python@3.12.14 uv@0.12.10 -- uv run --project python \
  python spikes/polars-interop/differential.py \
  spikes/polars-interop/target/debug/lvu-polars-interop
```

Suitable root task names are `expr-python-test`, `expr-rust-test`, and
`expr-interop-test`, with the commands above and `expr-interop-test` depending on
the other two. The primary owns `mise.toml`; this assignment does not edit it.

## Integration boundary

Reusable production behavior is in `python/lvu_expr_helper`. The Rust runner is an
isolated compatibility gate, not a competing query-engine API. Production Rust
should carry forward the compatibility-ID and installed-version checks, complete
line/inner-expression/depth limits, positive typed structural validator,
and native `Expr` deserialization/evaluation. Helper timeout, cancellation,
restart, OS resource limits, and stderr collection remain host responsibilities as
specified by the shared contract.
