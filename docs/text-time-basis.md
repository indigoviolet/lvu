# A text enrichment column as the event-time basis

Companion to `docs/field-exploration.md`. Closes the TODO row that read:
`ColumnTimeInterpretation::Text` requires an explicit chrono format and
`lvu_live::TimeInterpretation::Text` carries none, so a text column cannot
round-trip through a field token.

## What changed, in one paragraph

`TimeFieldSelection` gained `text_format: Option<String>`, encoded as a fifth
`|` part of its token; four-part tokens still parse, so a persisted selection
from before this change keeps working and says what it is missing. The format
is *inferred* from the same bounded sample the Time dialog already reads,
*measured* by the parser that will actually run, and *shown* in the
confirmation step with its match rate and a sample value, in a field the user
can correct. `TimeFieldError::FormatRequired` is therefore a state with a
suggestion rather than a refusal.

## Where each half lives, and why

`lvu` cannot depend on `lvu-live` or `lvu-query` — they depend on it — so the
dialog holds a token and a draft, and asks. That constraint shapes everything
here:

| Step | Crate | What it does |
| --- | --- | --- |
| Rank the shapes | `lvu-live::time::infer_text_time_formats` | Hand-written matchers over the value's own bytes, reusing `scan_datetime`/`scan_syslog`. No chrono: this crate has none. Ranking only. |
| Measure a format | `lvu-view::time_basis::validate_text_column` | Builds a one-column frame and runs `lvu_query::validate_time_basis`. The rate the user sees comes from here. |
| Offer the readings | `lvu-app::time_recognition` | One `TimeFieldCandidate` per shape, best coverage leading, the rest as the override. |
| Show, edit, re-measure | `lvu::components::time` | The `Format` field, and `TextFormatProbe` back through the same outbox the recognition request uses. |

The split matters: **the matcher never reports a rate.** It decides which
formats are worth measuring; Polars decides what they read. A shape the
matcher liked but Polars cannot compile shows as reading nothing, which is
true, rather than as a confident wrong number.

## The shapes

| Shape | Format | Offered |
| --- | --- | --- |
| RFC 3339 | `%+` | yes — reads `Z`, `+02:00` and `+0200` alike, which the spelled-out forms do not |
| ISO with a space and an offset | `%Y-%m-%d %H:%M:%S%.f%#z` | yes |
| ISO without a zone | `%Y-%m-%dT%H:%M:%S%.f` | yes, under a declared zone |
| date and time without a zone | `%Y-%m-%d %H:%M:%S%.f` | yes, under a declared zone |
| Apache / common log format | `%d/%b/%Y:%H:%M:%S %z` | yes |
| syslog | `%b %e %H:%M:%S` | **no** — recognised and explained |

Epoch digits are deliberately not a text shape. A column whose values are all
digits already goes down the epoch path, which states the unit; reading the
same digits as `%s` would silently fix that unit at seconds.

Syslog is the one shape that is recognised but not offered. `Mmm D HH:MM:SS`
carries no year and a Polars format has no way to supply one, where this
crate's own row-local reader takes it from `TimeFieldSelection::assumed_year`.
Naming it in the diagnostics is more use than omitting the column, which is
what the dialog did before.

## Assumptions and refusals

- A format that reads its own offset cannot also take a zone assumption
  (`ZoneAlreadyRead`); one that does not can only be read under a declared
  zone, and that declaration is an assumption the confirmation step makes the
  user accept. Neither is applied unasked.
- `|` separates the parts of a token, so a format containing one would not
  survive the round trip it exists for. It is refused in the field where it is
  typed, without a round trip, so the message can name the character.
- Formats are bounded at 64 bytes, the same bound `lvu-query` enforces.

## Rows the format cannot read

They become nulls with a count, never dropped. `validate_time_basis` answers
for every row it is given; `time_basis_unix_nanos` returns one entry per row;
`lvu-view` adds the misses to `event_time_invalid`, which the membership
diagnostic already reports. A record with no readable time has no time *in
this basis* — it is still in All events, which is unfiltered.

## Decisions that are the user's

1. **The inferred format is applied only after the confirmation step.** It
   could lead straight to an applied basis when the rate is 100%.
   Recommendation: keep the confirmation. A format is a claim about what the
   characters mean, and 100% of a 256-row sample is not 100% of the data.
2. **The probe re-uses the recognition request rather than a new channel.**
   One outbox, one generation fence, one completion path. Recommendation:
   keep it; a second channel would need its own fence for no gain.
3. **The matcher is hand-written rather than chrono-based.** `lvu-live` has no
   chrono and adding it for ranking would pull a dependency into a crate that
   deliberately hand-rolls its scanners. Recommendation: keep it, and keep
   `every_inferred_shape_parses_under_polars` as the test that stops the two
   from drifting.

## Tests

- `crates/lvu-view/tests/text_time_basis.rs`: every offered shape really
  parses under Polars, a foreign value lowers the measured rate, the token
  round-trips, a four-part token still parses, syslog is marked unreadable and
  really does read nothing, unreadable rows stay as nulls, and a format that
  could not survive a token is refused.
- `crates/lvu-app/src/time_recognition.rs` unit tests: a text column is
  offered with its inferred format, a partial read is still a basis, a
  zone-less shape carries its assumption, syslog is explained in the
  diagnostics, and a numeric column is still read as an epoch.
- `crates/lvu/tests/component_time_text_format.rs`: the dialog shows the
  format with its sample and rate, a partial rate is acceptable, typing alone
  asks for nothing, Enter asks for a measurement, a measured edit replaces the
  reading it was an edit of, and `|` is refused in the field.
- `tests/pty/test_text_time_basis_pty.py`: the same on a real terminal, from
  an enrichment step through the basis dropdown to an edit that is measured.
