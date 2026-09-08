# Field, type and value exploration — audit against `dialog-system.md` §8.11–§8.13

Status: on main since preview 052. Companion to
`dialog-default-actions.md` (§8.9) and `dialog-discoverability.md` (§8.10).

The three rules in one paragraph each:

- **§8.11 Structured values.** A JSON record is a tree in Details and in
  Fields: containers collapse to `{3 keys}` / `[12]` and open in place with
  Enter or Right (Left closes, or climbs from a leaf); expansion is
  remembered per view and per path; scalars are the record's own bytes, keys
  are decoded only for identity, nothing is re-serialised or reordered, and
  a lossy byte stays the replacement character the provider substituted.
- **§8.12 Value exploration.** Fields carries a fixed-height Value pane for
  the selected path: inferred type with its share of the sample and the
  first sample value with its record, present/sampled counts, distinct
  count (floor at 4,096), the five top values with counts, min…max for
  numbers and timestamps — over the first 2,048 unfolded records, named in
  the pane's heading. Six one-key actions act on the selected value.
- **§8.13 Field path picker.** The editors' completion popup offers every
  nested scalar path of the sampled records (depth ≤ 4) and inserts the
  expression that reads it, so a path is never typed by hand.

## What each surface shows

| Surface | Before | Now |
| --- | --- | --- |
| Details, JSON record | `key: value` for the *top-level scalars only*; nested objects and arrays were dropped by field recognition and only visible inside the `raw:` line | `stable display id`, `raw`, then the tree: `  ▸ http: {3 keys}` collapsed, `  ▾ http: {3 keys}` open with children indented; scalars in the log line's JSON colours; the cursor row `›` while the pane has focus |
| Details, non-JSON record | `key: value` rows | unchanged; Up/Down scroll as before |
| Details keys | Up/Down scroll one line | Up/Down move the cursor (scroll follows), Enter toggles, Right opens, Left closes or climbs; `Expand or collapse value` in the palette (Enter shown in Details focus) |
| Fields list | top-level scalars, `[ ] name  value`, alphabetical | the same tree as Details, in the record's own key order (a JSON record is never reordered; logfmt records keep the recognised order), with a disclosure column for containers; the checkbox is blank for nested rows because pinning acts on the top-level column |
| Fields Value pane | none | `Value · path` heading with `first 2,048 records`; Type · confidence, Sample · record, Present, Distinct, Range, Top |
| Fields actions | `[ Pin ] [ Color rows by field ] [ Correlate across sources ]` | `[ _P_in ] [ _F_ilter ] [ E_x_clude ] [ _C_olor ] [ Fol_d_ ] [ Co_r_relate ]`; class M → class L; two panes side by side at ≥ 72 content columns, stacked below |
| Advanced filter and step editor completion | top-level fields as `pl.col("name")`; sampled literals | plus nested paths, indented, marked `(nested · JSON path)`, inserting `pl.col('http').str.json_path_match('$.status')`; an unspellable key is marked `(nested · extracted lexically)` and inserts the `str.extract` form |

## The actions, and what a nested value acts through

The query side (`lvu-query/src/adapter.rs`) makes only top-level JSON keys
columns and keeps nested values as JSON text inside their top-level column;
Polars' `extract_jsonpath` feature gives that column `str.json_path_match`.
So:

| Action | Top-level value | Nested value |
| --- | --- | --- |
| Pin | pins the column | pins the top-level column |
| Color | colours by the column | by the top-level column |
| Fold | `fold_key_column = column`, folding on, minimum run defaulted, expanded runs cleared — what choosing the column in the Folding dialog does | the top-level column |
| Correlate | the column | the top-level column |
| Filter | `pl.col('status') == 200` typed by kind; strings quoted and decoded; `is_null()` for null; a logfmt field compares as a string | `pl.col('http').str.json_path_match('$.status').cast(pl.Float64, strict=False) == 200` — the leaf by JSON path, numbers as numbers, strings/booleans/null as the text the match returns; a key the path cannot spell (quote or backslash) falls back to the lexical pair match |
| Exclude | `!=` / `is_not_null()` | `!=` on the same match; a record without the path is null there and drops out either way |

Filter and Exclude write the Advanced draft, joined with `&` to the applied
expression when there is one, reset its caret, and submit through
`Views::enqueue`; a refusal (full queue, fixed definition) is worded in the
status line. The user can open `p` and see exactly what was submitted.

## Bounds

| What | Bound | Where |
| --- | --- | --- |
| Tree | 16 KiB, 2,048 tokens, depth 64 — `json_spans::classify`'s | `json_tree` returns no tree beyond them; the flat rows stay |
| Expansion memory | 256 paths per view | `MAX_EXPANDED_PATHS`; session only |
| Statistics sample | first 2,048 unfolded records | `MAX_STATS_ROWS`; named in the pane heading |
| Distinct values | 4,096, then a floor | `MAX_DISTINCT_VALUES` |
| Value key | 512 bytes, longer count as `(long value)` | `field_stats` |
| Picker paths | 128 rows × depth 4 × 128 paths | `MAX_COMPLETION_*` |
| Statistics recompute | once per (view, provider revision, path) | `FieldsDialog::stats_for` |

## Decisions that are the user's

1. **Nested filtering and extraction are exact** (follow-up, done).
   `extract_jsonpath` is enabled in `crates/lvu-query/Cargo.toml`; it pulls
   in Polars' vendored `jsonpath_lib` and recompiled 39 crates (the Polars
   stack) in 9m38s on the shared host at load 12, and grows the debug
   `lvu-app` from 389,822,112 to 393,015,488 bytes (+3.2 MB, +0.8%).
   `value_predicate` and `nested_path_expression` build the JSON-path form;
   `json_tree::json_path` spells the path (`$.tags[1]`, `$['b c']`) and
   returns `None` for a key with a quote or backslash, the one case that
   keeps the lexical form. Exactness is tested end to end: a nested `503`
   does not match `5033` and `"slow"` does not match `"slower"`.
2. **Expansion memory is per session.** It could persist with the view as
   an additive `PresentationState` field (no schema move); recommendation:
   leave it per session until a use shows up, because a restored view with
   stale open paths reads as noise.
3. **Filter to value writes the Advanced filter, not Search.** Search's
   `"field": text` is a substring match and cannot exclude or compare typed
   values. Recommendation: keep Advanced; it is the one editor whose draft
   the user can read back.
4. **The Value pane samples the first 2,048 records of the view, not the
   viewport.** Deterministic and cheap; a sample around the selection would
   follow the user but change under them. Recommendation: keep it, and say
   the bound in the heading (done).

## Tests

- `crates/lvu/src/json_tree.rs` unit tests: document order, summaries,
  per-path expansion, array indexes, verbatim bytes and decoded values,
  non-JSON and lossy input.
- `crates/lvu/src/field_stats.rs` unit tests: type inference from JSON
  kinds and from spelling.
- `crates/lvu/tests/field_exploration.rs`: Details tree and memory across
  records and into Fields, non-JSON fallback, Fields default action versus
  disclosure, the Value pane over the nested fixture at 80x24 and 54x16,
  Filter/Exclude/Fold effects on the submitted query and the view, the
  predicate forms, the mnemonics and palette rows, and the picker's nested
  path insertion.
- `tests/pty/test_field_exploration_pty.py`: the same on a real terminal
  with a lossy byte in the source file.
