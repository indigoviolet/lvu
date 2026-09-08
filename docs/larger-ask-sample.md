# A wider bounded sample for short requests

Status: implemented on the local Sol continuation branch; not yet published.
See `docs/dialog-system.md` §12.17 for Ask 🧠 and
`crates/lvu-view/src/export/assistance.rs` for preparation limits.

## Current behavior

`A` opens Ask. Submitting a request freezes the applied view and prepares a
bounded typed sample before sending the prompt. The context travels inline.
Preparation reports coverage for each source, including available,
materialized and omitted rows. The Activity pane shows the current answer's
sample size, source count and tier, including when all available rows fit.

A finished standard-tier answer or error offers **Ask again with a wider
sample** when coverage indicates omitted rows or the answer reports
`needs_more_data`. The user chooses whether to spend another agent turn.
The action submits the same request text with a new generation and a freshly
prepared wider sample. Stale results remain fenced by generation. A wider
answer does not offer another escalation.

| Tier | Total samples | Samples per source | Scanned records | Inline context |
| --- | --- | --- | --- | --- |
| Standard | 512 | 128 | 50,000 | 32 KiB |
| Wider | 2,048 | 512 | 250,000 | 96 KiB |

These are ceilings; byte limits can admit fewer rows. The bridge caps the
whole proposal prompt at 128 KiB, so the wider context leaves room for
instructions and the response schema. The limits remain bounded for both
tiers and do not adapt to each source's size. Investigation continues to use
its separate fixed-snapshot workflow.

## Answer coverage and remaining history work

The dialog associates the displayed answer with its sample metadata. Starting
a wider request clears the previous answer and its displayed metadata; the
current implementation does not retain a transcript comparing both answers.
The original proposal's persistent per-answer sample history, including an
Investigation transcript line, is therefore still open work.

## Validation

W14 reported passing Rust workspace tests, clippy, bridge typecheck/build and
87 bridge tests. Its initial PTY matrix passed 60/66 with inherited
`NO_COLOR=1`; all six affected suites passed sequentially against the same
binaries after clearing that variable. These are worker results; combined
primary validation is tracked separately in `docs/work-ledger.md`.
