# Dialog presentation

Every visual treatment has one meaning. Dialogs use the shared theme roles and
share their rendered geometry with input, scrolling, hitboxes and text selection.

| Content | Presentation |
| --- | --- |
| Editable value | Clear field label, input background, cursor inside the visible field |
| Field help | Readable normal-contrast examples below applied state; no repeated shortcut inventory |
| State | Explicit No filter, Updating, Applied or Error; no empty applied row |
| Accepted value | Labeled read-only content, retained when a draft differs or fails |
| Results and diagnostics | Separate labeled pane with a bounded, inspectable viewport |
| Actions | Focusable action buttons or a compact contextual action row |

Help groups commands by task and aligns shortcut/action columns. It adapts its
column count to terminal width, and its scroll input belongs to the modal rather
than the log behind it. Long diagnostics must remain reachable through scrolling
or an explicit disclosure; clipping is not a substitute for an inspection path.

Footers contain actions, not scan summaries, timing details or errors. Moving a
shortcut out of body text must retain its discoverability. Narrow layouts must
preserve editable rows and the selected result before allocating extra padding.
Selection stays within the visible modal interior, including the footer.

Validation uses both Ratatui buffers and real PTYs: inspect color/bold roles,
cursor position, narrow layout, scrolling, accepted-state preservation and
terminal cleanup. Color tests explicitly clear inherited NO_COLOR and request
truecolor; production respects the user's color preference.

Search uses its dialog title once; the input is followed by applied state and then
examples. The main status aligns with the log pane, while the small activity heart
sits in the bottom-left interior of the continuous sidebar border.

## Consistent form layout — follow-up required after preview038

These rules apply to every dialog, not only Time. Group related fields on one
row when they fit, then reflow whole groups on narrow terminals. Input styling
covers the actual input rectangle; it must not connect unrelated controls into
a large background slab. Actions have visible button boundaries, distinct focus,
and matching mouse hitboxes. Use dropdowns for finite choices where appropriate.

Place Applied/Updating/Error in a separate status block with semantic colors and
explicit labels, so color is not the only signal. Secondary help follows status
and remains readable. Do not repeat titles or universal-key instructions.
Scroll affordances exist only for actual overflow; hidden or disabled controls
must not occupy visual space or keyboard traversal. Keep needed diagnostics
reachable without PgUp/PgDn/Home/End bindings.

Audit Time and enrichment first, then Source/Discovery, Search/advanced,
command enrichment, Settings, Recipes, Views, Bookmarks/notes, assistance and
investigation. Inspect read-only Fields/Details/Context/Storage too, applying
only relevant distinctions rather than adding unnecessary controls.

### Contrast audit after preview038

Muted foreground is for decoration or unavailable actions, never information the
user needs to read. Focus may change a border or action highlight, but must not be
required to read status, explanations, paths or field names. Inspect both dark
and light themes, with the content focused and unfocused.

| Surface | Required follow-up |
| --- | --- |
| Discovery | Base foreground for path/scan diagnostics; published in preview039 |
| Time | Separate applied state from help; grouped Start/End; bounded buttons and actual overflow only |
| Enrichment | Readable status; input/output hierarchy; visible Add/Edit/Remove and External command actions |
| External command | Readable review/status and help; distinguish definition, review, execution and results |
| Settings | Readable save/restart status; distinct values versus actions |
| Storage | Readable scan/cleanup status; preserve error color and diagnostic scrolling |
| Details | Readable field labels and no-selection message, including when unfocused |
| Search/advanced | Preserve existing input/Applied/help ordering; readable empty-state and status labels |
| Source, Recipes, Views, Bookmarks, assistance | Audit control grouping, actual button bounds and finite-choice controls with shared focus geometry |

These are outstanding audit items, not assertions that the current published
binary implements them. Keep successful controls and transactions intact while
correcting presentation.

## Time form — grouped layout published in preview039

The Time form uses Time basis and Window dropdowns. The absolute
range has Start and End rows, each with date, time and timezone inputs. Tab and
Shift-Tab traverse controls; arrows navigate open dropdowns; Enter activates the
focused control; Escape dismisses the active dropdown before the dialog. These
universal keys are not printed as instructions or placed in a decorative footer.

Existing unfinished drafts take precedence. Otherwise seed from accepted resolved
bounds, then the opening selection in the chosen time basis; use a clearly defined
clock default only when neither exists. Opening/prefilling never applies a filter.
The opening record anchor remains stable while live data arrives. Missing event
or extracted timestamps must not silently become capture timestamps.

Show Apply, Clear and timestamp-recognition actions as focusable buttons. Keep
Applied/Updating/Error separate from controls, and place readable contextual help
below status. A disabled choice explains why. Retain custom restored rolling
intervals rather than silently replacing them with a preset.

Initial timezone entry accepts UTC and explicit numeric offsets, normalized to the
existing UTC nanosecond query boundary. Preserve fractional precision and reject
calendar errors, invalid offsets and overflow without changing the accepted view.
Named timezone/DST conversion is not claimed by this initial form.

Preview039 groups Start [date] [time] [timezone] and End equivalents,
with separate Apply/Clear/Recognize actions and overflow controls only when needed.
Timezone uses a staged
UTC/numeric-offset dropdown with custom-offset entry. Preserve restored custom
offsets, nanoseconds and drafts; do not imply named-zone or DST support.

Acceptance includes prefill without submission, draft restoration, dropdown and
mouse behavior, date/time/zone focus and visible cursor on small terminals, offset
normalization, nanosecond round trips, invalid-input rollback, rolling policy
persistence, stable selected anchors and timestamp-assistant revision fencing.

## Keyboard policy — published in preview037

Do not bind operations to PgUp, PgDn, Home or End. Use focused arrow navigation,
scrolling and visible controls, retaining access to long diagnostics and results.
Do not print Enter, Tab or Escape reminders; their conventional interaction still
works. A dropdown owns arrow keys and closes before the containing dialog. Audit
both command discovery and real terminal workflows when replacing a binding.

## Dialog surface audit — rendered evidence, main @ 1d0c44f

This section records what the dialogs actually render today at four terminal
sizes. It is findings only: no policy, no target geometry. Every claim below is
taken from a Ratatui `TestBackend` capture of the real `ui::render` path with
populated dialog state (a nine-entry storage snapshot, six recipes, an accepted
Source 🧠 proposal, an accepted Ask 🧠 proposal, a bookmark note, a filled
External command definition), not from reading the code.

Sizes: **140x40** (roomy), **100x30** (typical), **80x24** (the classic default),
**54x16** (small split pane). `surface` below is `hit_regions.selection_modal`,
the rect that rendering, mouse hitboxes and text selection all share.

The unedited screens are in `dialog-audit-captures.md`; the fragments quoted
below are excerpts from it.

### 1. Every dialog rect is a hardcoded row count

`ui::centered(area, percent_width, height)` scales the **width** with the
terminal and takes the **height** from a per-dialog constant. Nothing consults
how much content there is.

| Surface | Rect expression | 140x40 | 100x30 | 80x24 | 54x16 |
| --- | --- | --- | --- | --- | --- |
| Command palette | `min(w,92) × min(h,24)` | 92x24 | 92x24 | 80x24 | 54x16 |
| Help | `centered(94%, min(h-2,30))` | 131x30 | 94x28 | 75x22 | 50x14 |
| Raw context (`o`) | `centered(116%, 26)` | 140x26 | 100x26 | 80x24 | 54x16 |
| Settings | `centered(104%, 30)` | 140x30 | 100x30 | 80x24 | 54x16 |
| Source (manual/discovery/🧠) | `centered(104%, 24)` | 140x24 | 100x24 | 80x24 | 54x16 |
| Ask 🧠 | `centered(94%, 24)` | 131x24 | 94x24 | 75x24 | 50x16 |
| Investigation 🧠 | `centered(94%, 24)` | 131x24 | 94x24 | 75x24 | 50x16 |
| External command | `centered(86%, 24)` | 120x24 | 86x24 | 68x24 | 46x16 |
| Enrichment editor | `centered(80%, 26)` | 112x26 | 80x26 | 64x24 | 43x16 |
| Bookmarks / notes | `centered(100%, 22)` | 140x22 | 100x22 | 80x22 | 54x16 |
| Time window | `centered(88%, 22)` | 123x22 | 88x22 | 70x22 | 47x16 |
| View sources | `centered(94%, 22)` | 131x22 | 94x22 | 75x22 | 50x16 |
| Storage usage | `centered(88%, 20)` | 123x20 | 88x20 | 70x20 | 47x16 |
| Named recipes | `centered(84%, 20)` | 117x20 | 84x20 | 67x20 | 45x16 |
| Event fields | `centered(70%, 16)` | 98x16 | 70x16 | 56x16 | 37x16 |
| Grouping | `centered(80%, 13)` | 112x13 | 80x13 | 64x13 | 43x13 |
| Editor completion | `centered(76%, 12)` | 106x12 | 76x12 | 60x12 | 41x12 |
| Search / Advanced filter | `centered(80%, 11)` | 112x11 | 80x11 | 64x11 | 43x11 |
| Source view prompt | `centered(76%, 10)` | 106x10 | 76x10 | 60x10 | 41x10 |

Consequences visible in the captures:

- At **140x40** no dialog uses more than 30 of 40 rows. Raw context uses 26,
  Source/Ask/Investigation/External command 24, Storage and Recipes 20, Event
  fields 16, Search 11. Ten to twenty-nine terminal rows sit unused *while the
  same dialog is clipping its own content*: Source 🧠 shows a "Preview" pane
  with two trailing blank rows, Recipes shows six blank rows, Bookmarks shows
  fourteen.
- Conversely the width is unbounded, so at 140 columns Settings spreads
  `Provider/model: … Mode: … Thinking: …` across 138 columns with ~30 blank
  columns between related values, and Search reserves 110 columns for a one-line
  query whose only body text is a 60-character example row.
- At **54x16** the fixed percentages leave dead gutters instead of using the
  screen: Storage is 47 wide in a 54-column terminal (7 columns wasted) while
  its own footer wraps onto a second line; Recipes is 45 wide (9 wasted);
  Event fields is 37 wide (17 wasted) while its footer wraps.

### 2. Two widgets are drawn into the same cells

**Source view prompt.** `render_dialog_text` paints the wrapped body into
`dialog_body(popup)` (interior minus one row) while `render_view_dialog_buttons`
paints into `popup.bottom()-4 .. popup.bottom()-1`. Those ranges overlap, so at
any width where the message wraps the buttons are interleaved into the message
glyph by glyph. At 54x16:

```
│ Name the view. Creating, cloning, and    │
│[ New blank ]s[ Clone ]s[ Rename ]ure.    │
│[ Sources ] [ Apply ]                     │
```

With a long name the same rect also receives two different windows of the value:

```
│ Name: ghijklmnopqrstuvwxyz0123456789M │
│ eventsABCDEFGHIJKLMN                  │
│[ New blank ]w[ Clone ]g[ Rename ] and │
│[ Sources ]r[ Apply ]e source capture. │
```

### 3. Every overflowing single-line input renders one wrong trailing character

The input rect is painted twice: first with a full-width window
(`clipped_width` / `input_tail` over `field.width`), then `place_input_cursor_at`
repaints only `field.width - 1` columns. The final column keeps the character
from the first paint. With the draft
`ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789` at 54x16:

| Surface | Rendered tail | Expected tail |
| --- | --- | --- |
| Bookmark note | `…vwxyz0123456789x` | `…vwxyz0123456789` |
| Search | `…klmnopqrstuvwxyz01234567899` | `…0123456789` |
| Add source path | `…klmnopqrstuvwxyz01234567899` | `…0123456789` |

The duplicated glyph is the character the caret sits over, so it also makes the
caret position ambiguous.

### 4. Overflow affordances are inconsistent and sometimes absent

Measured `dialog_scroll_limit` / `hit_regions.dialog_scroll` / `help_scroll_limit`:

| Surface | Overflow marker | Mouse scroll hitbox | Notes |
| --- | --- | --- | --- |
| Time window | `[ ▲ Scroll up ]` / `[ ▼ Scroll down ]` buttons | — | The only surface with visible, focusable scroll controls |
| Source 🧠 preview | pane title `Preview · lines 1–3 of 11 · ↑/↓` | yes (54x16 only) | Count in the title; only appears once it overflows |
| External command | pane title `Status and review · ↑/↓ scroll` | yes (54x16) | Different title convention from Source 🧠 |
| Storage | footer text `↑/↓ active pane` | yes, all sizes | No "n of m" anywhere; 4 of 9 entries silently missing at 54x16 |
| Discovery | none in the pane | yes, all sizes | Hitbox exists with no visible affordance |
| Ask 🧠 | `[ More ]` button in the header row | yes, but 3 rows at 54x16 vs 8 elsewhere | Request and State panes are not scrollable at all |
| Investigation | `[ More ]` button | yes, 1 row tall at 54x16 | |
| Help | footer text `↑/↓ or j/k scroll` | **none at any size** | `help_scroll_limit` is 2 / 22 / 29 / 58 — it really does overflow, but the mouse wheel has no target |
| **Recipes** | **none** | **none** | Only 5 of 6 recipes render at 54x16. The list *is* windowed on the selection, so ↑/↓ reaches the sixth — but nothing on screen says a sixth exists, there is no "n of m", and the wheel has no target. |
| **Bookmarks** | **none** | **none** | List is windowed with no indicator; the note is a tail-windowed single-line input |
| **Settings** | **none** | **none** | See §5 |
| Event fields | footer `↑/↓ select` | none | Uses its own viewport, not `dialog_scroll` |
| Details pane | footer `↑/↓ scroll` | none | 2 of 5 fields at 54x16, `raw:` value clipped mid-value |

Five different vocabularies for the same idea: focusable buttons, a count in a
pane title, `↑/↓ scroll` in a pane title, `↑/↓ …` in the dialog footer, and a
`[ More ]` button.

### 5. Settings loses information as the terminal narrows

- **80x24**: the `Effective values and paths` pane renders exactly one of its
  nine lines (`Effective 🧠 : codex/env [environment LVU_AI_PROVIDER] · full-access`,
  hard-clipped), with `dialog_scroll_limit == 0` and no scroll hitbox. Driving
  `Action::ScrollDialog(1)` four times changes nothing and leaves the limit at 0. The
  settings path, data path, cache path, capture path and startup-applied MiB
  values are simply not reachable at this size. They are all visible at 140x40.
- **54x16**: the dialog switches to a one-field-at-a-time compact form. Only
  `Provider/model` is shown; mode, thinking, theme, delight, reduced motion,
  ASCII and all four cache limits are behind a `[ More ]` button. The layout
  does not degrade — the information architecture changes.
- Label/value stutter throughout: `Saved: Saved; restart required for cache-limit changes`.
- At 140x40 an `input_bg` slab of 60 cells covers one row containing three
  unrelated fields *and* their labels, and the provider value inside it is
  tail-clipped to `ex/gpt-5.6-sol`.

### 6. Content is clipped mid-word with no ellipsis and no path to the rest

| Surface | Size | Rendered | Lost |
| --- | --- | --- | --- |
| Storage entries | 100x30 | `…unused, recomputable from capture re` | the `reclaimable` marker on 8 of 9 rows |
| Storage entries | 80x24 | `…api-fixture/00.rows.idx — unused, recomputab` | status of every row |
| Storage title | 54x16 | `Storage usage — total 50.0 MiB / unused deri` | the unused-derived figure |
| View sources headings | 54x16 | `Order: source position, then record sequence (` / `The owning source remains included; captures a` | both explanations, unwrapped, unscrollable |
| Recipes status | 54x16 | `Applied: 6 saved recipes · Enter applies` | the rest of the sentence |
| Source 🧠 help | 54x16 | `Describe a source; review is required before captu` | end of sentence |
| Ask 🧠 State | 54x16 | `Proposal: Proposal ready — review before` | end of sentence; pane has no scroll |
| External command | 54x16 | `Arguments: 1 line(s) · One argument per li` | help text — and the `TZ=UTC` **value row for Environment is dropped entirely** |
| Details `raw:` | 54x16 | `raw: fixture request 16` | ` completed` |
| Add source State | 54x16 | `Ready: provide a file path or command. Capture` | rest of sentence |

Note the priority inversion in External command: the field *help* survives the
clip and the field *value* is what disappears.

### 7. Labels, values and help are not visually separated

- **External command** puts label and help on one row and the editable value on
  the next, with no indentation or heading weight to separate the two:
  `Program: Executable path; no shell parsing` / `/usr/bin/jq`.
- **Recipes** stacks four unrelated kinds of content with no headings or rules:
  the list, a suggestion sentence, a debug-shaped preview line
  (`Preview search="" advanced=false enrichment=false pins= color=none capture-time=all`
  — implementation `key=value` jargon in an ordinary user control), and the
  status line.
- **Storage** flattens category, size, label and two status words into one
  run-on string per row (`> derived     1.0 MiB api-fixture/00.rows.idx — unused, recomputable from capture reclaimable`),
  so nothing can be column-aligned and the right-hand facts are the first to clip.
- **Bookmarks** shows `1 / 128 bookmarks ·` — a trailing separator with an empty
  status appended unconditionally.
- **Discovery** paints the input background across the `Search` *label* row.
- Status prefixes disagree across dialogs: `Ready:`, `Proposal:`, `Saved:`,
  `Status:`, `Applied`, `Error`, and `UPDATING:` in shouty caps in Discovery.

### 8. Input surfaces are slabs of inconsistent width

`input_bg` cell counts on the editable row, measured at 100x30 (modal is 98
columns wide):

| Surface | input_bg cells | Field content |
| --- | --- | --- |
| Source 🧠 request | 96 | one line |
| Bookmark note | 95 | one line |
| Add source path | 95 | one line |
| Discovery search | 88 | one line, **includes the label row** |
| Investigation question | 87 + 88 + 88 | one line, three rows of slab |
| Ask 🧠 request | 87 + 88 + 88 + 88 | one line, four rows of slab |
| External command fields | 81–82 | one line each |
| Search / Advanced / Grouping | 75 | one line |
| Settings cache limits | 76 / 67 | **two fields per painted row** |
| Source view name | 65 | one line |
| Time start/end rows | 62 | three sub-fields, correctly bounded |
| Settings 🧠 row | 60 | **three fields + labels in one slab** |

Time is the only form whose input styling tracks the actual input rectangles.

### 9. Button rows have no grouping and reflow into the dialog body

- **Recipes** renders eight equally-weighted buttons in one run:
  `[ Browse ] [ Save ] [ Import ] [ Export ] [ History ] [ Update ] [ Refresh ] [ Apply revision ]`.
  Navigation modes and destructive/committing actions are indistinguishable. It
  wraps to two rows at 100x30 and three at 54x16.
- **Source** moves its primary action between modes: manual shows
  `[ Manual ] [ Discover ] [ 🧠 ] [ File ] [ Command ]`, discovery shows
  `[ Manual ] [ Discover ] [ 🧠 ] [ Refresh ]`, and the 🧠 proposal shows
  `[ Start reviewed ] [ Manual ] [ Discover ] [ 🧠 ]` — the three shared mode
  buttons shift horizontally as soon as a proposal arrives.
- **Views** renders the same five buttons in both the name prompt and the
  sources list, but the last label changes width (`Apply` → `Apply membership`),
  changing where the row wraps.

### 10. Help

- `help_scroll_limit` is 2 / 22 / 29 / **58** at the four sizes, so at 80x24 the
  reader must scroll through roughly three screens; no scrollbar, no position
  indicator, and no mouse hitbox.
- The two-column layout switches on a hard `body.width >= 92`. The dialog is 94
  columns wide at 100x30 but its body is 90, so a 100-column terminal gets the
  single-column layout and 22 rows of scrolling.
- Wrapped descriptions return to column 1 instead of the description column,
  breaking the shortcut/description alignment the section already establishes:
  `  Alt-C in Enrichment  Add, edit, remove, or explicitly run` / `the terminal command step`.

### 11. Command palette

- Fixed `min(width, 92) × min(height, 24)`, so a 140x40 terminal leaves 48
  columns and 16 rows unused around a list that is already scrolling.
- The category column is left-aligned at a fixed x with ~40 trailing blank
  columns at 140x40.
- Category values are inconsistently cased in the same column: `Sources`,
  `Filter`, `Application`, `Views`, `Recipes`, `Agent` — and `agent` lowercase
  for `Ask agent` and `Investigations`.
- The selected-command detail block at the bottom has no border or rule, unlike
  the bordered `State` panes every dialog uses for the same job.

### 12. Small-terminal budget is consumed before the dialog is drawn

`ui::layout` keeps `SIDEBAR_WIDTH = 22` for any terminal ≥ 48 columns. At 54
columns the sidebar takes 22 and the log pane gets 30. Dialogs are then centred
over the whole 54, so the space pressure at 54x16 is a fixed cost paid by the
underlying layout, not by the dialogs. Below 48 columns the sidebar is dropped
entirely, and below 20x6 `render_tiny` prints `terminal too small` — that
fallback is correct and honest.

### Surfaces deliberately not audited here

- The **Enrichment editor** (`centered(80%, 26)`, plus its compact fallback) is
  being restructured into two layers by another assignment.
- The **Source 🧠 preview pane** interior is being changed by the concurrent
  scroll fix; only its outer rect and its clipping symptoms are recorded above.
