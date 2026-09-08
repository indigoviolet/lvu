# Raw context as a jump, not a dialog — design

Status: decided and built (2026-09-08); the user's words were "i can't really
see much of the filtered view under `o` dialog today anyway. i'd rather not
have another dialog which is a view." The dialog, `Focus::Context`, its state,
its three actions, its renderer and class XL are deleted; `RowProvider::
context_page` is kept for the docked pane discussed below. Deviations from
the design as written are listed at the end. Companion to
`docs/dialog-system.md` §12.12 (the dialog as it is), §12.20 (the docked
Details pane), `docs/component-model.md` §6.3 step 4 (Raw context was the last legacy
dialog once W14 converted Ask and Investigation) and
`docs/merged-view-ordering.md`.

## Why the dialog exists, and why it no longer needs to

`o` opens a class-XL, read-only window of the unfiltered physical stream
around the selected record, with `[ Back to anchor ]` (`g`) and ↑/↓ over a
bounded `context_page`. It predates the permanent All events view. Every
source now has a canonical All events view that shows the same raw,
unfiltered, ungrouped stream with everything a real view has: selection,
Details, Fields, bookmarks, folding, search within it, time navigation, the
readiness sentences, and a status line. The dialog is a second, weaker copy
of that view with its own scroll model, its own key list, no Enter default
(§8.9 audit), and `Focus::Context` plus `ContextDialogState::return_focus`
in the legacy half of `App`.

The user's question — "so why does `o` go to a dialog" — has one honest
answer: history. The proposal is that `o` becomes a **jump** to the same
record in the source's All events view, with a way back that restores
exactly what the user was looking at, including the dialog they pressed it
from.

## The interaction

### Vocabulary

- **Origin**: the view the user was in, the record that was selected there
  (the *anchor*), and, if a dialog was open, which one. One origin at a time,
  shell-owned, one level deep — the same shape as `pending_jump`.
- **Raw view**: the anchor's source's canonical All events view
  (`canonical_view_for_source`).
- **Return**: select the origin view, land on the anchor, re-open the origin
  dialog if there was one.

### Keys and controls

| Where | Key / control | Does |
| --- | --- | --- |
| Log pane, selector (origin is a filtered view) | `o` | Jump: switch to the raw view, chase the anchor into the middle of the viewport, select it. |
| Log pane, selector (already in All events, no origin) | `o` | Nothing but a notice: `this is the raw stream` (§8.10: an operation that cannot apply says so). |
| Raw view while an origin is held | `o` | Return. |
| Fields dialog | `[ Raw context ]` button, `o` | Close Fields, jump; origin records `Fields`. |
| Bookmarks dialog | `[ Raw context ]` button | Close Bookmarks, jump; origin records `Bookmarks`. (`Go to` is unchanged: it jumps to the canonical view and holds no origin — it is a destination, not a glance.) |
| Palette | `Raw context` / `Back to <view name>` | The same two operations, listed with `o` as their chord; the return entry is listed only while an origin is held. |

`o` is the one key in both directions on purpose: the user pressed it to
look, and presses it again to stop looking. Escape is not the way back,
because on the base screen Escape is the dismissal that quits, and a jump is
not a dialog to dismiss.

### The status line

While an origin is held, the raw view's status line carries one segment,
placed where the gap notice sits today (before the constraint indicators,
because it answers "what just happened"):

```
 FOLLOW off | raw of Enriched · #19 · o back | 12-31/64 | ? help · Ctrl-P commands
```

`raw of <origin view name> · #<anchor sequence> · o back`. The `o back` is
the one non-routine key the segment may print (§8.10: a key for an operation
that has no button and belongs to this screen alone). The segment is not a
transient notice; it lives as long as the origin does.

### Landing

The jump lands with the anchor **selected and centred**, `follow` off in the
raw view, exactly as `jump_to_record` does for a bookmark. The anchor is a
`RowId`, so the selection survives live appends and, once
`docs/merged-view-ordering.md` lands, reordering (its I6/I7).

### What "back" restores, per entry point

| Origin | On return |
| --- | --- |
| Log pane | Origin view selected; the anchor is the selected row again and is centred; `follow` stays off (it was turned off by the selection before the jump, or was already off). Scroll position other than "anchor centred" is not restored: the anchor is the thing the user was looking at, and centring it is the promise. |
| Fields | As above, then `Open::Fields` is pushed. Fields opens on the view's anchored record and the row/path the user had under the cursor, because those are view-owned (§7.3 accepted debt: `ViewState` keeps the Fields anchor, selected row, control and open paths). The Value pane recomputes; its cache is geometry, not state. |
| Bookmarks | As above, then `Open::Bookmarks` is pushed with the same bookmark selected. This needs the selected bookmark to be view-owned like the Fields selection is; today it is component-owned, so the conversion moves it (§7.3 pattern). |

W15's constraint — both dialog callers return to the dialog that opened
them — is met by re-pushing the dialog rather than by keeping it on the
stack: a layer cannot stay open under a different view (§6.4 forbids a
layer over the base of another view, and both dialogs read the active view).
Re-pushing is the only version that keeps §1's stack discipline, and it is
why the state the dialogs show after return must be view-owned.

If the origin has become unreachable — the origin view was closed, or the
anchor's source was removed — return does what it can and says so: select
the origin view if it exists, otherwise stay, and retire the origin with a
notice (`origin view was closed`). It never reopens a dialog it cannot
seed.

### When the origin is retired

- Return (consumed).
- Any deliberate navigation away from the raw view: switching view, opening
  a source, applying a recipe, jumping to a bookmark or gap in another view.
  Scrolling, selecting, folding, searching, opening Details/Fields/Bookmarks
  *inside* the raw view do not retire it — looking around is the point.
- A second `o` from a filtered view replaces it (one deep, newest wins).
- Quit. The origin is not persisted; it is a glance, not state.

## Composition with the dialog rules

- **§8.9 default action.** Nothing changes: `Pin` stays Fields' default,
  `Go to` stays Bookmarks'. `Raw context` is an ordinary button in both. The
  dialog's own §8.9 problem — `Back to anchor` unreachable by Enter —
  disappears with the dialog.
- **§7.5 buttons.** The label stays `Raw context` with no trailing `…`: it
  no longer opens a child dialog.
- **§8.10 discoverability.** The two operations live in the palette with
  their chord; the status segment prints `o back` under the "non-routine key
  with no button" allowance; no other hint appears anywhere.
- **§8.11–§8.13.** Unchanged. The raw view is a real view, so the Details
  tree, the Value pane, one-key filters and the path picker all work on the
  raw stream, which the dialog could not offer. The Fields cursor and open
  paths survive the round trip because they are per view.
- **§5.1 classes.** Class XL loses its only member. The row stays in the
  table until a second XL dialog exists or the class is retired; retiring it
  is a one-line edit and not this design's call.
- **§12.12** is deleted; its sketches move to a "Retired" note so the
  reasoning is not lost.

## Readiness: All events has not indexed the record yet

A jump reuses `pending_jump`: `index_of_id` is asked once per frame, bounded
by `MAX_JUMP_ATTEMPTS` (240 frames), and the viewport is not pinned forever.
Meanwhile the raw view renders whatever `RowReadiness` says — `Indexing
{ indexed_records, reported_records }`, `RowsPending`, `IndexContended`,
`Stalled` — in place of a blank pane, as every view must (§5.2.1, the
readiness sentences). The status segment shows `raw of Enriched · #19 ·
locating…` until the anchor resolves, then `o back`.

If the bound is hit, the jump keeps the raw view and the origin, selects
nothing, and says `record #19 is not addressable in All events yet`. `o`
still returns. This is the same outcome the dialog gives today through
`ContextPage::pending` and `diagnostic`, with the difference that the user is
in a view that will finish indexing in front of them rather than in a window
that has to be reopened.

## Merged views

A merged view shows several sources; a record belongs to one. `o` on a
merged view jumps to **that record's source's** All events, which is the only
raw stream the record has (`All events always shows exactly its own
source`). The neighbours the user sees are the source's physical neighbours,
which is what the dialog shows today (`context_page` "must not cross
sources") and what "raw context" means. The return restores the merged view
and its anchor, and under `docs/merged-view-ordering.md` I7 that anchor
resolves to the same record at whatever merged position it now has.

The status segment names the origin view, not the source, so `raw of Merged
api+worker · #19` reads correctly. A merged view has no All events of its
own and this design does not invent one: the interleaved raw stream *is* the
merged view with its filters cleared, which the user can do with the
existing editors.

## What is lost against the dialog

The dialog shows the raw neighbourhood **over** the filtered view, which stays
visible underneath (scrimmed) and returns intact on close. The jump replaces
the whole log pane. Three consequences:

1. **No side-by-side glance.** The user cannot see the filtered rows and the
   raw rows at once. This is the real loss.
2. **The filtered view's scroll position** is not restored, only the anchor.
   In practice the dialog restored a scrimmed, non-interactive copy of it, so
   nothing the user could act on is lost.
3. **The "unfiltered, ungrouped" framing** in the dialog's header goes; the
   raw view's status line says `All events` and its own counts, which is the
   same fact in the vocabulary every view uses.

**Would a docked pane like `d` recover the glance?** Yes: a `Raw context`
pane docked below or beside the log, fed by `context_page` (or by a second
viewport over the raw view's `RowProvider`), scrolling with the selection,
would show the physical neighbours of whichever record is selected while
the filtered list stays interactive above it. It would be the read-only
dialog turned into a docked pane with the Details pane's rules (§12.20).

**Is it worth it?** Not as part of this change. The Details pane already
shows the selected record's raw line; what the pane would add is the
*neighbours*, and the question is how often a user needs neighbours and
the filtered list in the same glance rather than a jump and a return. Two
signals argue for waiting: the user's own question was why `o` is a dialog
at all, not why it is not docked; and a docked pane is a second
`RowProvider` viewport with its own readiness, scroll, selection and
resize rules — the cost of the Details pane again. Build the jump, keep
`context_page` in the provider trait so the pane stays a possible follow-up,
and revisit if the glance is missed in use.

## Component-model change

§6.3 step 4 becomes a **deletion**, not a conversion:

- Remove `Focus::Context`, `ContextDialogState` (and its `return_focus`,
  the last dialog-owned "where I came from" outside Help's, which §6.5 step
  6 already retired), `Action::OpenContext`, `Action::OpenContextForLayer`,
  `Action::MoveContext`, the `Focus::Context` arms in `handle`,
  `handle_mouse`, `key_to_action`, `cancel_editor` and the render dispatch,
  and `ui::render_context_dialog`.
- Add to the shell (`App`, base state, not a layer): `Origin { view_id,
  anchor: RowId, layer: Option<Open> }` in one `Option`, and two actions:
  `Action::RawContext` (jump) and `Action::ReturnFromRawContext`. The jump
  is `select_view` + `pending_jump` + `Origin` set; the return is
  `select_view` + `pending_jump` + `push_layer(open)` if `layer` is `Some`
  + `Origin` cleared. Both are routing-only arms (§7.7); the work is in
  functions that already exist.
- Fields and Bookmarks: `Outcome::Defer(Action::OpenContextForLayer(anchor))`
  becomes close-then-`Outcome::Legacy(Action::RawContext { anchor, layer:
  Some(Open::Fields) })` — the shape Bookmarks' `Go to` already uses. With
  Correlation converted (`component-model.md` §6.5), `Legacy` no longer
  opens any dialog; it survives only for these shell hand-offs.
- Bookmarks: move the selected bookmark into `ViewState` so `Open::Bookmarks`
  reopens on it (Fields already keeps its selection there).
- `RowProvider::context_page` and `CommandRows`' delegation stay until the
  docked-pane question is settled. `ContextPage` is dead code in the UI once
  the dialog goes; a `#[deprecated]` note on the trait method says why it is
  kept.
- Status line: an `origin` segment beside `gap_notice`, drawn by the same
  code path; palette catalog: two entries with availability reasons
  (`already the raw stream`, `nothing to return to`).
- Tests: the `Focus::Context` cases in `ui_state.rs` become jump/return
  cases (log pane, Fields, Bookmarks, unreachable origin, readiness bound,
  merged view); `test_lvu_real_pty.py`'s raw-context steps become a jump and
  a return through Fields.

Unconverted after this: only Correlation, converted next (component-model.md
§6.3 step 14). The legacy `Focus` enum loses one variant and `App` loses one
dialog state struct.

## Recommendation

Retire the dialog and make `o` a jump with a return: it puts the raw stream
in the one place that already renders it completely, gives the raw
neighbourhood every capability a view has (Details tree, Fields, one-key
filters, folding, bookmarks, readiness sentences), deletes a legacy focus and
a dialog-owned return-focus field, and keeps W15's constraint by re-pushing
the origin dialog from view-owned state rather than holding it open across
a view switch. The one real loss, the side-by-side glance, is recoverable
later by a docked pane over the same `context_page` seam, and the evidence
that it is missed should come from use, not from keeping a dialog nobody
asked for.

**Alternative A — convert the dialog as planned (§6.3 step 4).** Keep the XL
dialog, give it §8.9's Enter default and the message row, and convert it to
a component with `Replace` semantics. Cost: one more converted dialog to
maintain that duplicates a view; benefit: the glance stays and no jump/return
state is introduced. Choose this if the glance is known to matter more than
the raw view's capabilities.

**Alternative B — jump now, docked pane later.** The recommendation above,
with the docked `Raw context` pane scheduled rather than deferred: same
deletions, and the pane is specified against §12.20 at the same time so
`context_page` is designed for rather than merely kept. Choose this if the
user already knows the glance is what they use `o` for.

## As built

- `Action::RawContext { anchor, layer }` is the jump (the keymap's `o`, the
  palette's `Raw context`, and Fields'/Bookmarks' button with their anchor
  and `Open`), and the shell turns it into the return when an origin is held
  in the active raw view. `Action::ReturnFromRawContext` is the palette's
  `Back from raw context`; both rows print `o`.
- `App::raw_context_origin` holds `{ view_id, raw_view_id, anchor, layer }`.
  It is retired before every action and by the status line whenever the
  active view is no longer the raw view, which covers switching views,
  `Go to` from Bookmarks, recipes and source changes without listing them.
- Landing reuses the bookmark jump: `select_view`, selection by identity,
  `pending_jump` chased into the middle of the viewport, bounded at 240
  frames; the bound's notice now reads `record #N is not addressable in this
  view yet` for either caller.
- The status segment is `raw of <origin view> · #<sequence> · o back`, or
  `· locating…` while the jump is pending.
- Bookmarks' selected bookmark moved to `ViewState.bookmark_selected`,
  written when the dialog closes or jumps and read when it opens, so the
  return re-pushes it on the same row. Fields already kept its anchor and
  cursor in the view.
- `o` on the raw stream with nothing to return to says `this is the raw
  stream · o returns nowhere`; from a dialog it also re-pushes that dialog.
  A source with no All events view, or an origin view that was closed, says
  so and does nothing else.
- Class XL had no other member and is gone from `DialogClass`.
- **Readiness in a real terminal.** The `locating…` state is shown while
  `pending_jump` is unresolved. In the PTY story the record of a 400,000-line
  source resolved within one frame, because the canonical view shares the
  source's index with the filtered view that found the record: by the time a
  filtered row is on screen, All events can address it too. The state is
  therefore proven by the Rust test that hides the raw view's rows
  (`a_record_the_raw_view_cannot_address_stops_being_chased_and_says_so`)
  and only observed opportunistically by the PTY story, which prints what it
  saw. The case it exists for is `RowsPending` between frames and a source
  whose index is being rebuilt.
- **A source without an All events view** (the demo fixture, a workspace
  restored before roles were recorded) gets `this source has no All events
  view to show`, and a dialog that asked comes back.
