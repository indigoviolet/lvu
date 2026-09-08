# A larger bounded sample when the first one was not enough — design

Status: design only; nothing here is built. Companion to
`docs/dialog-system.md` §12.17 (Ask 🧠 as it is), `docs/component-model.md`
§6.3 step 12 (Ask is an owned component with an agent outbox) and
`crates/lvu-view/src/export/assistance.rs` (the preparation that produces the
sample).

## What happens today

`A` opens Ask, the user writes a request, and `Submit` freezes the applied
view and prepares a **bounded typed sample** of it before any prompt is sent.
The bound is `AssistancePreparationLimits::default()`: at most 512 samples
across sources, 128 per source, 50,000 records scanned, and a serialised
context that must fit in 32 KiB. The whole context travels inline with the
proposal request; `prepared_sample_context` refuses anything larger.

The preparation already knows, per source, exactly how much it left out. It
emits a `coverage` array with `available_rows`, `candidate_rows`,
`materialized_rows`, `omitted_rows`, `omitted_values` and `omitted_raw`.

Two things are missing. The user never sees any of that — a proposal built
from 128 of 4,000,000 rows looks exactly like one built from all of them. And
there is no way to answer "that wasn't enough" other than rephrasing the
request and hoping, which re-runs the identical bound.

## What the agent can already say

An answer can fail for two distinguishable reasons, and the design needs both:

- **The sample was demonstrably thin.** `omitted_rows > 0` is known locally,
  before the prompt is sent and regardless of what comes back. This is a fact
  about the preparation, not an opinion.
- **The answer says it needs more.** The bridge's proposal envelope can carry
  an insufficiency signal from the agent itself ("the sample contained no
  `ERROR` rows, so I cannot characterise them"). This is the agent's opinion
  and is only available after a turn.

The first is cheap and always available; the second is precise but costs a
turn. The design uses both: the first to *show* the size, the second to
*offer* the escalation.

## The proposal

### One escalation, not a ladder

Ask gains a second preparation tier, and exactly one step to it.

| Tier | samples | per source | scanned records | inline context |
| --- | --- | --- | --- | --- |
| Standard (today) | 512 | 128 | 50,000 | 32 KiB |
| Wider | 2,048 | 512 | 250,000 | 128 KiB |

The wider tier is a cap, not a door: there is no third tier and no "use
everything". A request that genuinely needs the whole capture is an
**Investigation**, which is what the fixed snapshot and the bounded
schema/sample helper already exist for. Ask stays the short-request dialog.

### What the user sees

The Activity pane gains one line, always, not only on escalation:

```
Sample     128 of 4,201,993 rows · 3 sources · standard
```

and after escalation:

```
Sample     512 of 4,201,993 rows · 3 sources · wider
```

When the sample was thin or the answer said it needed more, the message row
says so in the §7.4 vocabulary and the actions gain one control:

```
○ Ready     This answer used 128 of 4,201,993 rows.
[ Apply ]  [ Ask again with a wider sample ]  [ Cancel ]
```

`[ Ask again with a wider sample ]` re-submits the *same request text*
against a freshly prepared wider sample. It is a new turn with a new
generation, fenced exactly as the first one is, and it is offered only once
per request: after a wider answer the control is gone.

### The transcript records which sample each answer used

Ask's Proposal pane keeps one line per answer naming its tier and size, so a
user comparing two answers can see that they were not asked the same
question. Investigation's transcript records the same line at the turn that
used it. The record is per *answer*, not per dialog: escalating does not
rewrite what the first answer was based on.

## Decisions that are the user's

These are UX and policy choices, not implementation ones. My recommendation is
first in each list, with the reason.

**1. When is the wider control offered?**

- **On either signal (recommended).** Offer it whenever `omitted_rows > 0`
  *or* the answer reports insufficiency. Costs nothing when unused, and the
  common case — a big capture, a thin sample, a plausible-looking wrong answer
  — is exactly the one where the user has no reason to suspect anything.
- Only when the agent asks. Fewer controls, but it depends on the agent
  reliably admitting it lacked data, which is the least reliable thing in the
  loop.
- Always, whenever a wider tier exists. Simplest rule, but it puts a
  second-guess button under answers that used every row there was.

**2. Does escalation re-run automatically?**

- **No; the user presses it (recommended).** A wider preparation scans up to
  250,000 records and costs a second agent turn. Silent retries spend the
  user's time and the provider's budget without being asked.
- Yes, once, when the agent reports insufficiency. Fewer keystrokes, but it
  makes one `Submit` mean up to two turns, and the dialog would have to
  explain that afterwards rather than before.

**3. What is the wider tier's size?**

- **4× samples, 5× scan, 128 KiB inline (recommended).** Big enough to change
  an answer, small enough that preparation stays interactive and the context
  stays inline rather than needing the snapshot path.
- 2×. Safer for latency, but often not a different answer, which makes the
  control feel like a no-op.
- 8× or more. Approaches the point where the honest move is an Investigation
  with a real snapshot, and the inline context would have to become a file.

**4. Is the sample line always shown, or only when something was omitted?**

- **Always (recommended).** "128 of 128 rows" is information: it tells the
  user the answer saw everything. Showing the line only on shortfall teaches
  the user to read its absence as "fine", which is the same as not showing it.
- Only on shortfall. Quieter dialog; costs the user the ability to distinguish
  "complete" from "not measured".

## What this does not do

- It does not read the whole capture. Both tiers are bounded and the second
  one is the last.
- It does not change what Investigation does. Investigation still freezes a
  snapshot and gets the bounded schema/sample helper for full-data reads;
  this is only about the short-request path.
- It does not make the sample adaptive per source. A source with 4,000,000
  rows and one with 12 get the same per-source cap, as today.
