# lvu dialog system

Status: design specification, not implemented. Supersedes the layout guidance in
`dialog-design.md`; the *controls* rules there (bounded buttons, visible caret,
no universal-key reminders, no PgUp/PgDn/Home/End bindings, semantic roles) stay
in force and are reconciled in §8. An implementer should be able to build every
surface from this document without further interpretation.

Evidence: real-PTY screens of every dialog at 140x40, 100x30, 80x24 and 54x16
(`dialog-system-captures.md`, empty workspace) and the populated-state
`TestBackend` audit on main (`dialog-audit-captures.md`, findings in
`dialog-design.md` §"Dialog surface audit"). Cell-style dumps and WCAG contrast
ratios quoted below were measured on those runs in both themes.

---

## 1. Diagnosis — why `t` reads well and the rest do not

The Time dialog is the only surface that has a *form*: a label column, a field
column, related fields on one row, one action row, one status line, one help
line, in a fixed order with blank rows between groups.

```
│ [ Time basis: Capture ▾ ]                                            │
│ [ Window: All time ▾ ]                                               │
│                                                                      │
│ Start 2026-09-07 02:12:44.343236213                 UTC      [ ▾ ]   │
│ End   2026-09-07 02:13:44.343236213                 UTC      [ ▾ ]   │
│                                                                      │
│ [ Apply ] [ Clear ] [ 🧠 Recognize timestamp ]                       │
│                                                                      │
│ ┌ Applied ─────────────────────────────────────────────────────────┐ │
│ │Applied: all times                                                │ │
│ └──────────────────────────────────────────────────────────────────┘ │
│                                                                      │
│ Bounds are half-open. UTC and numeric offsets are normalized to UT   │
│ C; named zones are not supported.                                    │
```

What it gets right, and what every other dialog lacks in at least one respect:

| Property | Time | Others (cited) |
| --- | --- | --- |
| Fields have a label column and values line up | `Start`/`End` share x for date, time, zone | External command puts label, help and value on alternating rows with no column (`Program: Executable path; no shell parsing` / value below). Settings puts three fields on one row 30 columns apart at 140 wide. Search has no label at all. |
| Input styling covers only the input rect | 62 cells over three sub-fields | Ask paints 4 rows × 88 cells for one line; Settings paints one 60-cell slab across three fields *and their labels*; Discovery paints the slab over the `Search` label row (`dialog-design.md` §8). |
| Actions are one row, after the fields | `[ Apply ] [ Clear ] [ 🧠 Recognize timestamp ]` | Enrichment, Ask and Investigation put the primary action **above** the fields (`[ Submit ]` before anything is typed). Source, Recipes, Bookmarks put buttons at the very bottom under 14–17 blank rows. Recipes has eight equal buttons wrapping to two rows. |
| Status is one labelled line, separated from controls | `Applied: all times` | `Applied  No filter applied.` (Search, two spaces), `Applied: Grouping disabled.` (Grouping), `Status` heading then `Applied: …` (Enrichment), boxed `State` / `Ready: …` (Source, Ask, Investigation), boxed `Status` / `Status: …` (Storage), `Saved: Saved; …` (Settings). Six vocabularies for the same row. |
| Help is one readable sentence below status | half-open bounds sentence | Help lives in pane titles (`Saved steps · kept when you add`, `Add step · name = expression OR /regex …`), in dialog titles (`Discover sources — selection never auto-starts`, `External command · runs only when confirmed`), inline after labels, or in the footer as key lists. |
| Blank rows separate groups | yes | Grouping stacks label, input, status with no gap; Views runs help straight into buttons and at 54x16 the buttons are drawn **into** the help text (`[ New blank ]s[ Clone ]s[ Rename ]ure.`). |

What Time still gets wrong, and the system below fixes: the status sits in a
bordered box for one line (box-in-box); the box title repeats the word
(`Applied` / `Applied:`); dropdowns are drawn as buttons with the label inside
(`[ Time basis: Capture ▾ ]`), so they do not share the label column with
`Start`/`End`; the zone dropdown is split from its value (`UTC      [ ▾ ]`);
help wraps mid-word (`UT` / `C` at 80 columns); the height is a constant 22, so
at 100x30 seven interior rows are empty while at 54x16 two of fourteen rows are
spent on `[ ▲ Scroll up ]` / `[ ▼ Scroll down ]` pseudo-buttons.

Cross-cutting defects, all visible in the captures:

1. **Surfaces do not separate.** Dialog background vs. base background is
   1.07:1 (dark) and 1.09:1 (light); input background vs. dialog background is
   1.16:1 and 1.17:1. Only border glyphs tell the user where a dialog or an
   input is. In the text captures the inputs are literally invisible.
2. **Boxes inside boxes.** Time (1), Enrichment (3, two side by side),
   External command (1), Source (1–2), Settings (2), Storage (1), Ask (3),
   Investigation (3). Most hold a single line. Some borders carry semantic
   colour (green `Applied`, muted `Proposal and activity`) which reads as noise.
3. **Titles carry data and warnings.** `Storage usage — total 134.8 KiB /
   unused derived 0 B`, `Ask 🧠 for a source — preview never executes`,
   `Command palette · Ctrl-P`, `Raw context · filter unchanged`.
4. **Three label conventions in one product.** ALL CAPS (`FILE PATH`, `FILTER
   EXPRESSION`, `Mode: CLONE SETTINGS`), inline colon (`Program:`, `Name:`),
   sentence case standalone (`Continuation regex over raw bytes`, `Request`).
5. **Fixed heights, percentage widths.** Source is 100% wide and 24 tall with
   16 empty rows; Search is 80% wide and 11 tall for one line of input; at 140x40
   nothing uses more than 30 of 40 rows while Recipes, Bookmarks and Storage clip
   their own lists (`dialog-design.md` §1).
6. **Five overflow vocabularies, three dialogs with none** (`dialog-design.md`
   §4). Settings drops its paths entirely at 80x24 and changes information
   architecture at 54x16 (§5).
7. **Rendering bugs**: two widgets painting the same cells in the View prompt
   (§2); every overflowing single-line input renders a duplicated trailing glyph
   (§3); Settings clips the provider value at the *start* (`xture/provider`).
8. **Button rows mix modes and actions.** Source: `[ Manual ] [ Discover ] [ 🧠 ]
   [ File ] [ Command ]` is two mode switchers and zero actions. Enrichment:
   `[ Steps ] [ Editor ]` (views) next to `[ Add ] [ Edit ] [ Remove ]` (actions).
9. **Footers disagree.** Read-only dialogs print key lists (`↑/↓ scroll · g
   anchor`, `↑/↓ active pane · r refresh · c preview/confirm cleanup`); forms
   print buttons; Bookmarks prints both, misaligned by one column.

---

## 2. Principles

1. **One anatomy.** Every dialog is title → header → body → message → help →
   actions, in that order, with nothing else. Regions that are empty take no rows.
2. **Surfaces separate by tone, not by borders.** The backdrop is dimmed, the
   dialog is a lighter/darker surface, inputs are a third tone. One border per
   dialog. Secondary content inside a dialog is a *pane* with a heading, never a
   second bordered box.
3. **Forms are two columns.** Labels left, sentence case, no colon; fields right,
   aligned; related fields share a row and reflow as a group.
4. **Size follows content, bounded by class.** Width is a function of the
   terminal; height is a function of content, capped by class. Empty rows are
   never padding for symmetry; scrolling replaces clipping.
5. **One action row.** Buttons only, after the content, the default first and
   filled (§8.9). No key-reminder footers. Modes are a segmented control in
   the header, never buttons in the action row.
6. **One message row.** A glyph, a state word, a sentence. Never boxed, never
   repeated, never in the title.
7. **Every rect is computable** from `(terminal area, class, content)` so that
   rendering, hit-testing, scrolling and selection share it.

---

## 3. Anatomy and region order

```
┌ Title ─────────────────────────────────────────┐   border row (title in it)
│                                                │   pad-top      (1 or 0)
│   Manual │ Discover │ 🧠 Agent                 │   HEADER       (0–1 rows)
│                                                │   gap          (1 or 0)
│  Label      value                              │   BODY         (flex; the only scrolling region)
│  Label      value                              │
│                                                │   gap
│  ● Applied  sentence                           │   MESSAGE      (1–2 rows)
│  Help sentence.                                │   HELP         (0–2 rows)
│                                                │   gap
│  [ Primary ]  [ Secondary ]  [ Destructive ]   │   ACTIONS      (0–2 rows)
│                                                │   pad-bottom   (1 or 0)
└────────────────────────────────────────────────┘
```

| Region | Rule |
| --- | --- |
| Title | `Name` or `Name · object` (e.g. `Bookmarks · Raw events`). A noun. Never a warning, count, shortcut or state. Rendered in the top border, `active_border` colour, bold. Child dialogs use a breadcrumb: `Enrichment › External command`. |
| Header | Only a segmented mode control (§8.6) or, for inspection dialogs, one summary line. Sticky: never scrolls. |
| Body | Form rows, lists and panes. Scrolls as one region when it does not fit (§9). |
| Message | Exactly one per dialog that has state. `glyph␣State␣␣sentence` (§7.4). Sticky. |
| Help | 0–2 rows of base-foreground sentences, word-wrapped. Only when it says something the field itself cannot. Dropped first under height pressure. |
| Actions | Buttons only (§8.2). Sticky. Wraps to a second row only when the first cannot hold them; overflow beyond two rows goes into `[ More ▾ ]`. |

Regions are separated by **gap** rows and the dialog has **pad** rows; both are
1 when the dialog's interior height is ≥ 14, otherwise 0 (§4.1). There is no
horizontal rule glyph between regions.

Dialog-level region rects (all derived from `popup`):

```
interior = popup.inner(Margin{1,1})                // inside the border
content  = interior.inner(Margin{horizontal: 1, vertical: 0})  // 1-col side padding → 2 cols from border
pad      = if interior.height >= 14 { 1 } else { 0 }
gap      = pad
actions_h = rows needed by ActionsRow at content.width (0 if no actions)
message_h = wrapped rows of the message sentence (1..=2), 0 if the dialog has no state
help_h    = if interior.height >= 10 { wrapped rows (0..=2) } else { 0 }
header_h  = 0 | 1
fixed     = pad*2 + header_h + (header_h>0)*gap + message_h + help_h
          + (message_h+help_h>0)*gap + actions_h + (actions_h>0)*gap
body_h    = interior.height - fixed              // if < 3, drop help, then drop gaps, then pads, in that order
```

Rows are assigned top-down in the order listed; the body is the remainder. This
is the *only* layout function dialogs use; a dialog supplies `header`, `body`,
`message`, `help`, `actions` content and gets back rects.

---

## 4. Layout

### 4.1 Spacing scale

| Token | Value | Use |
| --- | --- | --- |
| `side` | 2 columns | Border to content (1 border + 1 pad). Applies to every row including buttons and pane content. Nothing is flush against the border. |
| `pad` | 1 row / 0 | First and last interior rows when interior height ≥ 14. |
| `gap` | 1 row / 0 | Between regions and between form groups. Never two consecutive blank rows. |
| `gutter` | 2 columns | Label column to field column; between sub-fields on one row; between side-by-side panes; between buttons. |
| `indent` | 2 columns | Pane content under its heading; list rows under a list heading. |

### 4.2 Label and field columns

- `label_w = min(max(width(label) for labels in the dialog), 18)`. Labels are
  sentence case, no trailing colon, `base_fg`, not bold. The focused row's label
  is bold `accent`.
- Field column starts at `content.x + label_w + gutter`. Every field in the
  dialog starts there, including dropdowns and multi-line inputs.
- A field's width is either **fill** (to `content.right()`) for free text, or
  **fixed** for typed values: date 10, time 18, zone 10, number 8, dropdown =
  `max(longest option) + 4` (min 12). Fixed-width fields never stretch to fill.
- Fields on one row are separated by `gutter`. If the row does not fit
  `content.width`, the **whole group** reflows to one field per row and each
  reflowed label is the full compound name (`Start date`, `Start time`,
  `Start zone`), never `time` alone.
- When `content.width < label_w + gutter + 20`, labels stack above their fields
  (label row, then field row, no blank between); the label is then bold.
- Single-field prompts (class S) have no label; the field spans the content
  width and shows a placeholder when empty.

### 4.3 Grouping

- A **group** is consecutive rows with no gap (e.g. `Start` and `End`; the
  three cache-limit rows). Groups are separated by one gap row.
- A **section heading** (Settings: `🧠 Agent`, `Appearance`, `Cache limits (MiB)`)
  is a bold `base_fg` row at `content.x`, followed directly by its rows. It is
  the only bold body text besides pane headings.
- **Panes** (§8.7) are used for read-mostly secondary content: lists, previews,
  diagnostics, transcripts. A pane is a heading row plus an indented viewport;
  it has no border. Two panes may share a row (Enrichment raw/output) when
  `content.width ≥ 72`; otherwise they stack.
- **Lists** are panes whose rows are selectable; the heading's right side shows
  the count (`3 of 12`), right-aligned to `content.right()`.

### 4.4 Alignment

- Text, labels, headings, buttons and message glyphs all start at `content.x`.
- Numbers in list columns right-align within their column (Storage sizes).
- Counts and secondary facts in headings right-align to `content.right()`.
- Wrapping is word-based (`Wrap { trim: true }` semantics); a wrapped
  continuation of a labelled row starts at the field column, of a list row at
  the row's text column (Help's description column), never at column 0.
- Wide (2-cell) and combining characters count by display width for every
  column computation (`unicode-width`); 🧠 is width 2 and every width table
  above assumes it.

---

## 5. Sizing

### 5.1 Classes

`W`, `H` are the full frame `area` (the same rect `ui::layout` receives).
`compact = W < 64 || H < 20`.

| Class | Purpose | Width | Max height | Vertical anchor |
| --- | --- | --- | --- | --- |
| **S** Prompt | One field, live effect | `clamp(W·60%, 48, 72)` | 12 | top-biased: `y = area.y + max(1, H/6)` |
| **M** Form | Short form or short list | `clamp(W·72%, 60, 96)` | `H − 4` | centred |
| **L** Workspace | Multi-part form, long list, transcript | `clamp(W·86%, 72, 132)` | `H − 2` | centred |
| **XL** Inspector | Read-mostly content that benefits from every column | `W − 2` | `H − 2` | centred |
| **P** Palette | Command palette | `clamp(W·64%, 50, 92)` | `H − 3` | top-biased as S |
| **A** Anchored | Dropdown lists, completion popups | `max(field.width, 12)`, ≤ `W − field.x − 1` | `min(items, 8) + 2` | below the field; above it if there is no room below |

In `compact` terminals S, M, L and P become `W − 2` wide (XL stays `W − 2`);
max height becomes `H − 2` for S/M and `H` for L/XL/P. A dialog never exceeds
the frame and never drops below 20 columns; below `ui::layout`'s 20x6 floor the
existing `terminal too small` fallback applies.

### 5.2 Height follows *stable* content

```
fn dialog_rect(area, class, content) -> Rect
  width  = class width rule (above)
  rows   = content.rows_at(width - 4)     // the dialog measures its own regions at this content width
  height = min(rows + 2, class max height)  // +2 for the border
  x      = area.x + (area.width - width) / 2
  y      = per class anchor
```

`content.rows_at` is the §3 region arithmetic with `body_h` replaced by the
body's *natural* height (all form rows; list rows capped at 12; pane viewports
at their natural height capped at 8; multi-line inputs at their visible-row
cap). When `rows + 2` exceeds the class max, the body scrolls (§9) — the
header, message and actions never do. This is the whole answer to "empty rows
under Source" and to "Settings unreachable at 80x24": Source shrinks to its
content; Settings keeps every field and scrolls.

The content a dialog sizes to is the content that is *stable while the dialog
is open*. Content that changes under the user's fingers is measured by §5.2.1
instead, and only then handed to `dialog_rect`.

### 5.2.1 Live regions have a reserved height

A **live region** is one whose content changes while the user types: a path or
field completion list, a discovery candidate list, a live preview of the record
an expression reads. A live region gets a fixed row count chosen from the frame
and the size class when the dialog opens, never from the number of items it
currently holds.

```
fn live_rows(area, class, stable, desired) -> u16
  interior = class.max_height(area) - 2
  spare    = interior - stable.interior_rows()   // stable = the same content with the live region at 0
  rows     = clamp(min(desired, spare), min(MIN_LIVE_ROWS, spare), spare)
```

- **Overflow**: the region scrolls inside its reserved rows and says how much it
  is holding — the §8.7 pane count (`n of m`, `N matches`) plus the §9
  scrollbar, or a trailing `+N more` line where there is no pane heading.
- **Underflow**: the unused rows stay blank. A pending scan, an empty result and
  a full list all occupy the same rows. A region that is reserved from the
  moment the dialog opens keeps its §8.7 heading and says what it is for
  (`Suggestions` / `type a path to see matching files`) rather than reading as
  a block of dead space.
- **Consequence**: the popup rect is identical between keystrokes. Nothing in
  the dialog — its border, its buttons, the field the user is typing into —
  moves because a background scan returned a different number of answers.

A multi-line input the user is editing grows with its own text up to its §8.1
cap; that is feedback about characters the user can see, not a background
result. The dialog reserves room for that growth — as much of the cap as the
frame affords, by the same `live_rows` arithmetic — and the regions below the
field sit at the reserved offset rather than at the field's current bottom.

Content that changes only in response to a deliberate act — switching the mode
segment, submitting an 🧠 request, accepting a step — is stable in this sense
and still sizes to content under §5.2. The test is whether a *keystroke in a
text field* can change the region's height.

Applies to: Add source (suggestions pane, discovery candidates), the enrichment
step editor (expression field cap and its preview panes), Folding's key-column
picker (its rows come from a bounded sample of the view, so a still-arriving
source can add a column while the list is open), and any future region fed by a
debounced background scan.

The anchored class A popup (§5.1) is fixed-size *when its item count is*. Its
`min(items, 8) + 2` height is stable for a dropdown over a fixed option set —
every basis, window, zone and theme list — and live for one over a set the
product discovers. Such a list reserves its rows from the frame and passes the
reservation to `anchored_rect` in place of the item count; it cannot use
`live_rows`, whose spare-row arithmetic is for a region inside a body the dialog
must fit, because a popup is bounded by the frame and may extend past its own
dialog (§10). Its width comes from the field it drops from for the same reason:
the longest option is as live as the count.

### 5.3 Assignment

| Dialog | Key | Class | Why |
| --- | --- | --- | --- |
| Search | `/` | S | One live field. Must not cover the results it filters. |
| Advanced filter | `p` | S | One live field. |
| Multiline grouping | `m` | S | One field plus a 3-row preview pane. |
| Command palette | Ctrl-P | P | Transient, top-anchored, list-driven. |
| Time window | `t` | M | Six fields in four rows, three actions. Today's 88% width is 30 columns wider than its longest row at 100x30. |
| View | `v` | M | Name field, source membership list, three actions. |
| Recipes | `r` | M | List + name field + actions. |
| Bookmarks | `B` | M | List + actions. 100% width today for a 20-character row. |
| Fields | `i` | M | Two-column list; value column needs the width. |
| Folding | `z` | M | Four or five labelled rows, one anchored picker, one action. The picker is a live region (§5.2.1); the form rows are stable. `z` is vim's fold prefix and was unbound. |
| Note editor (Bookmarks child) | — | S | One field. |
| Enrichment | `e` | L | Steps list, add-step field, two panes. |
| External command (Enrichment child) | Alt-C | L (child) | Four fields, one of them multi-line, and a results pane. |
| Add source (all three modes) | `n` | L | Suggestions/candidates/proposal panes need rows. The suggestion and candidate lists are live regions (§5.2.1) and reserve theirs; the proposal pane still sizes to content. |
| Storage | `S` | L | Six-column entry list; rows are long. |
| Settings | `,` | L | Three sections plus an effective-values pane. Scrolls instead of hiding. |
| Help | `?` | L | Two-column reference. |
| Ask 🧠 | `A` | L | Multi-line request plus a proposal pane. |
| Investigation 🧠 | `I` | L | Multi-line question plus a transcript pane. |
| Raw context | `o` | XL | Record rows want every column; the user explicitly likes its use of space. |
| Details | `d` | not a dialog | Docked pane; §12 applies its label/value and scrollbar rules only. |
| Dropdown / completion | — | A | Anchored to the field. |

Resulting rects at the four sizes (non-compact unless noted):

| Class | 140x40 | 100x30 | 80x24 | 54x16 (compact) |
| --- | --- | --- | --- | --- |
| S | 72 × ≤12 | 60 × ≤12 | 48 × ≤12 | 52 × ≤14 |
| M | 96 × ≤36 | 72 × ≤26 | 60 × ≤20 | 52 × ≤14 |
| L | 120 × ≤38 | 86 × ≤28 | 72 × ≤22 | 52 × ≤16 |
| XL | 138 × ≤38 | 98 × ≤28 | 78 × ≤22 | 52 × ≤16 |
| P | 90 × ≤37 | 64 × ≤27 | 51 × ≤21 | 52 × ≤16 |

Heights are maxima; actual height is content-driven (§5.2).

### 5.4 Degradation order under height pressure

1. Drop `pad` and `gap` rows (interior < 14).
2. Drop help rows (interior < 10).
3. Cap list and pane viewports at 3 rows.
4. Scroll the body; keep header, message, actions.
5. Wrap the actions row to two rows; beyond that, move trailing non-primary
   buttons into `[ More ▾ ]`.

Under width pressure: reflow field groups (§4.2) → stack labels above fields →
stack side-by-side panes → truncate list cells with `…` at the end, never mid
value at the start.

### 5.5 Backdrop

`ui::layout` keeps the 22-column sidebar down to 48 columns, so at 54x16 the
dialog competes with a sidebar nobody can use. While any dialog is open in a
`compact` terminal the sidebar is not drawn and the log takes the full width;
the dialog is centred over the whole frame regardless.

---

## 6. Styling

### 6.1 Theme role changes (required)

| Role | love-dark today → new | love-light today → new | Requirement |
| --- | --- | --- | --- |
| `input_bg` | `#372531` → `#46303f` | `#efdad8` → `#e6c8c4` | ≥ 1.35:1 against `dialog_bg` (measured 1.39 / 1.37); `input_fg` on it ≥ 7:1 (10.9 / 8.2). |
| `dialog_bg` | unchanged `#261b24` | unchanged `#faedea` | Separation comes from the scrim (§6.2), not from lightening the surface. |
| `terminal` theme | `input_bg = DarkGray` unchanged | — | Already distinct; the scrim uses `Color::DarkGray` foreground. |

Every text colour continues to pass through `ensure_contrast(fg, bg, 4.5)`; the
new `input_bg` values were chosen so that `muted` placeholder text on them
(4.17 dark, 3.39 light) is lifted to 4.5 by that function rather than failing
silently.

### 6.2 Scrim

While a dialog is open, after the workspace is rendered and before the dialog
is drawn, every cell of `area` is restyled: `fg = theme.muted`, all modifiers
removed, `bg` unchanged. Severity colours, selection highlight and bold header
text all collapse to muted. Measured contrast of muted on base is 6.18 (dark)
and 5.05 (light), so the backdrop stays legible but clearly inactive. A nested
child applies the scrim again over its parent (§10). The scrim is a style pass
over the buffer, not a widget, and does not change any hit region.

### 6.3 Element roles

| Element | Style (both themes; colours are theme roles) |
| --- | --- |
| Dialog surface | `bg = dialog_bg`, `fg = base_fg` |
| Border, title | `active_border` for the frontmost dialog; `border` for a parent behind a child. Title bold. |
| Section heading, pane heading | `base_fg` bold |
| Label | `base_fg`; focused row's label `accent` bold |
| Field text | `input_fg` on `input_bg`; the painted rect is exactly the field rect |
| Placeholder | `muted` italic on `input_bg` (after `ensure_contrast`) |
| Caret | terminal cursor at the insertion point; shown only when an input has focus |
| Dropdown chevron `▾` | `accent` on `input_bg`, last cell of the field |
| Checkbox `[x]` / radio `●` | `base_fg`; the focused one `accent` bold |
| Segmented control | active segment `selection_fg` on `selection_bg` bold; inactive `base_fg`; separators `│` in `border` |
| Button, normal | `[ Label ]` in `base_fg` |
| Button, default (§8.9) | **filled**: `bg = accent`, `fg = dialog_bg` pushed through `ensure_contrast` to ≥ 4.5:1 (the terminal theme uses `selection_fg`), bold. One per dialog. |
| Button, destructive | `severity.error`; never the default |
| Button, focused | `selection_fg` on `selection_bg` bold (any kind, the default included: one focus ring everywhere, and the fill returns when focus leaves) |
| Button, disabled | `muted` italic, still rendered, still in the row; the reason appears in the message row while it is focused |
| List row, selected | full-row `selection_fg` on `selection_bg`; gutter glyph `›` always present so selection survives `NO_COLOR` |
| List row, normal | `base_fg` |
| Message glyph + state word | `severity.info` for Ready/Applied/Saved/Scanned; `severity.warn` for Pending/Updating; `severity.error` for Error; word bold |
| Message sentence, help text | `base_fg` (never muted) |
| Scrollbar | track `│` in `border`, thumb `█` in `accent`, arrows `▲`/`▼` in `border` (ASCII: `|`, `#`, `^`, `v`) |
| *At sixteen colours* | every role above is already an ANSI colour by the time a component reads it (`Theme::with_depth`). Surfaces lvu cannot know — `base_bg`, `dialog_bg`, `input_bg` — inherit the terminal's own; the two regions lvu paints both halves of, the selection and the §8.9 fill, take ANSI colours whose foreground clears 4.5:1 against the other half on xterm's palette. §8.10's mnemonic is an underline and needs no colour. |
| Error text inside a pane | `severity.error` |
| Unavailable/disabled choice in a dropdown | `muted` italic |

`muted` is used for: scrim, placeholders, disabled controls, scrollbar track.
Nothing the user must read to operate the dialog is muted.

### 6.4 ASCII fallback

`ascii = true` (settings) or `LVU_ASCII`: 🧠 → `Agent`; `●`/`○`/`◐` → `*`/`o`/`~`;
`✖` → `x`; `▾` → `v`; `›` → `>`; `│` separators → `|`; box drawing stays
(crossterm handles it); scrollbar glyphs per §6.3. Every label table below
lists the ASCII form where it differs.

---

## 7. Text conventions

### 7.1 Titles

Noun or `Noun · object`. Title case only on the first word. Examples: `Search`,
`Time window`, `Add source`, `View · Raw events`, `Bookmarks · Raw events`,
`Fields · record 19`, `Raw context · Raw events`, `Storage`, `Settings`, `Help`,
`Command palette`, `Ask 🧠`, `Investigation 🧠`, `Enrichment`,
`Enrichment › External command`, `Bookmarks › Note for #19`.

Banned in titles: shortcuts, counts, sizes, `—` clauses, warnings, state.

### 7.2 Labels

Sentence case, no colon, ≤ 18 cells: `Time basis`, `Window`, `Start`, `End`,
`Program`, `Arguments`, `Directory`, `Environment`, `Kind`, `Path`, `Filter`,
`Describe`, `Name`, `Provider / model`, `Mode`, `Thinking`, `Theme`, `Rows`,
`Membership`, `Derived total`, `Per source`, `Request`, `Question`.

### 7.3 Headings (sections and panes)

Sentence case nouns: `Steps`, `Raw input`, `Accepted output`, `Suggestions`,
`Candidates`, `Details`, `Proposal`, `Activity`, `Transcript`, `Saved recipes`,
`Bookmarks`, `Entries`, `Effective values and paths`, `Preview`,
`Results and review`, `🧠 Agent`, `Appearance`, `Cache limits (MiB)`.

### 7.4 Message row

```
●  Applied   request 1 · 10 of 64 records match
○  Ready     capture starts only when you open the source
◐  Updating  scanning 3 of 9 entries
✖  Error     advanced Polars adapter is not wired
```

- Column 1: glyph. Column 3: state word, bold, padded to 9 cells. Column 13:
  sentence, lower-case start, no trailing period, `·` between facts.
- State vocabulary (exhaustive): `Ready`, `Applied`, `No filter`, `Disabled`,
  `Pending`, `Updating`, `Saved`, `Scanned`, `Unrun`, `Error`.
- The sentence never repeats the state word (`Saved  restart applies cache
  limits`, not `Saved: Saved; restart required`).
- A dialog with state always shows this row, including the empty state
  (`○  No filter  showing all 64 records`). A read-only dialog without state
  (Help, Fields) has no message row.

### 7.5 Buttons

Verb or verb phrase, sentence case: `Apply`, `Clear`, `Save`, `Open`, `Rescan`,
`Submit`, `Start`, `Send`, `Add`, `Edit`, `Remove`, `Go to`, `Edit note`,
`Raw context`, `Pin`, `Color rows by field`, `Refresh`, `Preview cleanup`,
`Confirm cleanup`, `Request proposal`, `Start reviewed source`, `Review and run`,
`New line`, `Clone`, `New blank view`, `History`, `Update`, `More ▾`,
`🧠 Recognize timestamp`. A trailing `…` marks a button that opens a child
dialog (`External command…`). No `Cancel`, `Close` or `OK` buttons anywhere:
Escape closes, and the frontmost surface (dropdown, child, dialog) closes first.

---

## 8. Controls (reconciled with the existing contract)

### 8.1 Text input

- Painted exactly over its rect with `input_bg`; one paint pass. The
  duplicated-trailing-glyph bug (`dialog-design.md` §3) is fixed by rendering
  the visible window once and placing the caret without repainting.
- Overflow shows the **tail** with a leading `…` in `muted`; the caret is
  always inside the rect.
- Multi-line inputs have a visible-row cap (3 for Request/Question, 3 for
  Arguments/Environment) and scroll internally with a 1-column scrollbar at
  their right edge when the draft exceeds it. Enter inserts a newline only in
  multi-line inputs; `New line` remains available as a button for discoverability
  (Alt-N is its accelerator; never printed).
- Ctrl-A / Ctrl-K clear-to-ends stay as today.

### 8.2 Buttons

`[ Label ]` as in `dialog_controls`. Rows use `button_layout` with a 2-column
gutter. Every button has one of three **roles** (`dialog_controls::ButtonRole`):
`Default` (exactly one per dialog that has actions, filled per §6.3 and
executed by Enter per §8.9), `Normal`, or `Destructive` (last in the row,
styled error, never the default). The default is drawn first wherever the row
is a row of verbs; a row that still carries a mode set (View, until §12.8
lands) keeps the verb where it is and the fill, not the position, marks it. A
button that would start a third row moves into `[ More ▾ ]`, which opens an
**A**-class list of the remaining actions. Space never activates a button
(reserved for text); Enter on a focused button presses that button. Mouse
hitbox = the button rect.

A dialog declares its row as an `ActionRow { labels, default, destructive,
focused }` and draws it with `ui::render_actions`; `render_action_row` is the
same call with `default = Some(0)` for the common case where the default is
first. `role_style` is the only place a role becomes a style, so changing the
treatment is one edit.

### 8.3 Dropdown

Rendered as a field: value on `input_bg`, `▾` in the last cell. Enter or Space
(when focused, and the dialog has no text input focused) opens an **A** popup:
bordered in `active_border`, rows `base_fg`, selected row selection style,
scrollbar when > 8 options, unavailable options muted italic with the reason as
the popup's last row. Arrows move, Enter commits, Escape closes the popup
only. The previous `[ Label: value ▾ ]` button form is retired.

### 8.4 Checkbox and radio

`[x] Label` / `[ ] Label`; radios `● File  ○ Command` (ASCII `(*) File  ( ) Command`).
Space toggles the focused one; that is a checkbox convention, not a shortcut to
print. Several toggles may share one row separated by `gutter + 1` when they fit.

### 8.5 Lists

A pane whose rows are selectable. Gutter of 2 cells (`› ` on the selected row,
`  ` otherwise) then columns aligned per §4.4. Up/Down move; Enter executes
the dialog's default action on the selected row (§8.9) — jump, apply, open,
edit — never a third thing. Mouse click selects; double-click activates. Count
in the heading.

**Initial selection.** A list never opens with nothing selected while it has
rows. It opens on the row the opening context names — the anchor record (Raw
context), the step or bookmark the user was on (Enrichment reopened, Recipes
returning from History), the current value (every dropdown, the theme list),
the last-run command (palette after `refresh_context`) — and on the **first
row** otherwise. A persisted selection is clamped to the list it now indexes
before the first frame, so a list that shrank still opens on a real row. An
empty list shows its one-row empty state and the default action becomes the
verb that creates something (`Add`, `Save`, `Open`).

### 8.6 Segmented control (modes)

Header-only. `␣Manual␣│␣Discover␣│␣🧠 Agent␣` starting at `content.x`.
Left/Right (and click) switch modes when the control is focused; Tab reaches it
like any control. Used by Add source (`Manual │ Discover │ 🧠 Agent`) and by
Investigation (`New │ Saved`). Enrichment's `Steps │ Editor` toggle is removed:
one layout shows both.

### 8.7 Pane

```
  Heading                                              3 of 12
    row                                                     ▲
    row                                                     █
    row                                                     ▼
```

Heading bold at `content.x`; optional right-aligned count; content indented 2;
a 1-column scrollbar in the pane's last column **only when** content exceeds
the viewport. Never bordered. Two panes side by side split the content width
minus one gutter equally.

### 8.8 Focus traversal

Tab / Shift-Tab: header control → body controls top-to-bottom, left-to-right →
actions left-to-right. A focused control inside a scrolling body is scrolled
into view. Focus never lands on a pane unless it is a list or a scrollable pane
(then Up/Down scroll it). Mouse wheel over the body scrolls the body; over a
scrollable pane, that pane; over an anchored popup, the popup.

### 8.9 Default action and Enter

Every dialog that has an action row declares **one default action**. It is
the verb the dialog exists for — `Apply` for the editors and Time, `Save` for
the step editor and Settings, `Open` for Add source, `Go to` for Bookmarks,
`Pin` for Fields, `Submit`/`Start`/`Send` for the agent dialogs. A dialog
without an action row (Help) has no default and Enter is inert in it. A
destructive action is never the default, so Storage's default is `Refresh`
even while `Confirm cleanup` is showing, and Bookmarks' is `Go to`, not
`Remove`.

**Marking.** The default button is *filled* (§6.3: `accent` background, bold,
foreground pushed to ≥ 4.5:1) — the way a modern GUI marks its primary button
— and no other button in the dialog is. Nothing else changes: no glyph, no
different brackets, no extra label. When the focus ring is on the default it
takes the selection colours like any focused control; the fill returns the
moment focus leaves, and the one focus style stays one style.

**Enter executes the default** from anywhere in the dialog, with exactly these
exceptions, which are the controls that consume Enter themselves:

| Focus is on | Enter does | To execute the default instead |
| --- | --- | --- |
| A multi-line *prose* field (§8.1: Ask's Request, Investigation's Question, External command's Arguments and Environment) | inserts a newline | Ctrl-Enter (Ctrl-S also saves in External command; Tab to the button always works) |
| A multi-line *expression* field (Advanced filter, Multiline grouping, the enrichment step's Expression) | executes the default — these wrap for readability but are one expression, and Enter has always applied them | Alt-Enter (editors) / Alt-N (step editor) is their newline |
| An open anchored popup — dropdown list, completion list, `More ▾` menu | commits the highlighted row and closes the popup | press Enter again |
| A closed dropdown field | opens its list (§8.3) | Tab off it |
| A checkbox, radio or segmented control | toggles / selects the focused option, exactly as Space does | Tab off it |
| A button other than the default | presses *that* button | — |
| A pending confirmation surface (External command's run review) | confirms it; Escape drops it | — |

Everything else — a single-line text field, a list, a pane, a read-only
region, the default button itself — hands Enter to the default. Space never
executes the default (§8.2).

**A default that follows state.** The default may change with what the dialog
is showing, and when it does the fill moves with it; the labels do not change
and the buttons do not reorder. Enrichment is `Add` while the chain is empty
and `Edit` once a step is selected; External command is `Save` until a run
review is waiting, then `Review and run`; Ask is `Submit`, then `Apply` once a
proposal exists, then `Cancel request` while one is in flight; Investigation is
`Start`/`Resume`/`Send`; Recipes' single primary relabels by mode. The
component computes the default in one function that both `render` (which
button to fill) and the Enter handler (which verb to run) call, so the two
cannot disagree.

**What this means for the four legacy dialogs** (Raw context, Bookmarks, Ask,
Investigation), which are being converted while this rule lands: their
conversions must (1) name the default in one function used by both the render
and the Enter arm, (2) draw the row through `render_actions` with that index
(their `render_action_row` calls already fill index 0, which is the right
button in all four), (3) make Enter from every non-consuming control run it —
today Raw context ignores Enter (`Back to anchor` is unreachable except by
`g`), Ask and Investigation ignore it on their scroll panes, and the Note child
already submits — and (4) add Ctrl-Enter as the submit accelerator inside
their multi-line Request/Question fields, where plain Enter stays a newline.
The audit of every dialog against this rule is `dialog-default-actions.md`.

---

### 8.10 Discoverability — where the user learns what they can do

There are four places, each with one job, and a key or a sentence appears in
the one place that owns it. Dialogs do not print shortcut inventories, and
nothing prints the routine keys.

| Surface | Owns | May say | Never says |
| --- | --- | --- | --- |
| **Action row** | the operations of *this* dialog | the verb (§7.5); the Alt-letter that presses it, shown by underlining that letter in the label — the GUI mnemonic | a chord in text (`Alt-A`), an explanation, a key that is not Alt-letter |
| **Hint line** (the §3 help region; the base screen's status line) | what the content means and what a control cannot say for itself | syntax, consequence, state (`Pinned fields become log columns`); a non-routine key for an operation that has no button and belongs to this dialog alone (`Tab completes fields`) | routine keys; any Alt-letter (it is on the button); any operation the palette already names |
| **Command palette** (Ctrl-P) | the exhaustive index | every operation of the base screen and of the open layer, its real chord in the shortcut column, one description sentence; unavailable entries stay listed with their reason | anything else. It is the fallback of last resort, so it is complete and its chords are the ones that work |
| **Help** (`?`) | the base screen | the keys that have no visible control — navigation, toggles, the keys that open each dialog — grouped by area; one **Conventions** section that states the routine keys once | a dialog's own operations (those are buttons or palette rows); Alt-letters that a button shows |

**Routine keys** are Enter (the default action, §8.9), Esc (close the
frontmost thing, §5.3 / component-model §1), Tab and Shift-Tab (focus
traversal, §8.8), the arrows, and Space (toggle, §8.4). They mean the same
thing everywhere, so they are stated once, in Help's Conventions, and nowhere
else: no `Enter apply`, no `Esc close`, no `↑/↓ scroll`, no `Tab next`.

**Mnemonics.** A button label marks its accelerator letter with `&`
(`dialog_controls::mnemonic`): `&Add`, `&Edit`, `&Remove`, `External
&command…`. The marker is not drawn; the letter is underlined in every role and
focus state. Alt plus that letter presses the button whether or not it has
focus. The letter is a letter of the label — a chord that is not (`Alt-D` for
`Clone`) is not a mnemonic, so the button gains the mnemonic its label affords
and the old chord keeps working as an unlisted alias until it is retired. A
button with no Alt chord shows no underline; it is still reachable by Tab and
by the palette, which lists it without a shortcut. Ctrl-chords and single
letters inside a dialog are not mnemonics: they are listed by the palette.

**Every operation is reachable.** For each operation the app supports there is
a visible control (a button, a field, a segment, a list row) *or* a palette
entry with its chord, and usually both. A test walks the base keymap and
asserts each binding has a palette entry whose shortcut is that binding.

**The base screen.** The sidebar and the log pane carry no hints: their
headings are nouns and counts. The status line carries state — follow,
window, counts, the applied filters — and ends with the two doors, `? help ·
Ctrl-P commands`, which are the only chords printed on the base screen. The
docked Details pane has no footer. The `terminal too small` fallback keeps its
`q quit`, because it is a screen with exactly one operation.

### 8.11 Structured values — nested JSON

A record that is one JSON object is shown as a **tree**, in Details and in
Fields alike: each top-level key is a row; a value that is an object or an
array is a *container row* that collapses to a summary — `{3 keys}`, `{}`,
`[12]` — and opens in place to its children, indented one step per depth,
in document order. Scalars show the record's own bytes: `"INFO"`, `200`,
`null`, escapes and all. Nothing is re-serialised, reordered, pretty-printed
or decoded on the way to the screen; keys are decoded only for identity (the
same rule the log line's highlighting uses), and invalid UTF-8 arrives as the
replacement character the provider already substituted and stays that way.
The tree exists only inside `json_spans::classify`'s bounds (16 KiB, 2,048
tokens, depth 64); beyond them, or when the record is not one JSON value, the
flat `key: value` rows stay.

| Key | On a container row | On a scalar row |
| --- | --- | --- |
| Enter | opens or closes it (§8.9: a row with children consumes Enter, like a dropdown) | the dialog's default action (Fields: Pin); in Details, nothing |
| Right | opens it | — |
| Left | closes it | climbs to the container it sits in |
| Up / Down | moves the cursor | moves the cursor |

The disclosure glyph is `▸` closed and `▾` open (ASCII `>` / `v`); the
cursor row carries `›`. Expansion is remembered **per view and per path**
(`ViewState.expanded_paths`, bounded at 256 paths): opening `http.tags` in
Details keeps it open in Fields and for every record of the view that has
that path, for the life of the session. Enrichment `details` rows follow
the tree unchanged.

### 8.12 Value exploration

Fields carries a **Value pane** beside its list (below it under 72 columns
of content width) with a fixed height of eight lines, so moving the
selection never resizes the dialog (§5.2.1). It describes the selected path
over a bounded sample — the first 2,048 records of the view's *unfolded*
stream, named in the pane's heading — and says how much of the sample it
rests on:

```
  Value · http.status                              first 2,048 records
    Type       integer · 100% of present values
    Sample     200 · record 19
    Present    2,048 of 2,048 sampled records
    Distinct   4 values
    Range      200 … 503
    Top         1,203  200
                  512  404
```

- **Type** is the kind most present values have (`null`, `boolean`,
  `integer`, `number`, `timestamp`, `string`, `object`, `array`), judged from
  the JSON kind where the record is JSON and from the spelling otherwise,
  with the share of present values that agree. **Sample** is the first such
  value as spelled, and the record it came from.
- **Distinct** counts up to 4,096 values, then reports a floor (`4,096+`).
  **Top** lists the five most frequent with counts; ties keep first
  appearance. Values over 512 bytes count together as `(long value)`.
- **Range** is min … max for integers, numbers and timestamps (lexically,
  which is chronological for the ISO shapes the Time dialog accepts);
  `not numeric` otherwise.

The action row acts on the selected value in one key each, with the
mnemonics underlined (§8.10): `Pin` (default, Space), `Filter` (Alt-F),
`Exclude` (Alt-X), `Color` (Alt-C), `Fold` (Alt-D), `Correlate` (Alt-R).
Filter and Exclude write an Advanced filter — joined with `&` to the one
already applied, so the user sees and can edit exactly what was submitted —
of the shape `pl.col("status") == 200` (typed by the value's kind;
`is_null()` for null; a quoted literal for a recognised logfmt field). A
**nested** value acts through its top-level column, because the query side
holds nested values as JSON text in that column: Pin, Color, Fold and
Correlate use the top-level key, and Filter/Exclude address the leaf by
JSON path inside it — `pl.col('http').str.json_path_match('$.status')` —
compared typed: a number through `.cast(pl.Float64, strict=False) == 200`,
a string, boolean or null as the text the match returns. Equality is exact:
`'fast'` does not match `'faster'`, and `503` does not match `5033`. A key
the path syntax cannot spell (one containing a quote or a backslash) falls
back to the lexical pair match and the picker label says so. The pane's
`Value · path` heading and the help sentence say so. Fold sets the view's
fold key and turns folding on exactly as choosing that column in the
Folding dialog does.

### 8.13 Field path picker

A nested path is never typed by hand. The editors' completion popup
(Advanced filter on Tab, the enrichment step editor on Ctrl-Space) is the
picker: its `Complete field` list offers every top-level column as
`pl.col("name")` and, indented beneath, every scalar path the sampled
records carry to a depth of four — `  http.status  (nested · JSON path)` —
inserting the expression that reads that leaf from its top-level column's
JSON text:

```
pl.col('http').str.json_path_match('$.status')
```

The match is the value as text (a string without its quotes; a number,
boolean or null as spelled), which an enrichment step can cast. The sample
is the one the completion already reads (the visible rows, at most 128), so
the paths on offer are the paths on screen. A key the path syntax cannot
spell falls back to a lexical `str.extract` of the pair and is labelled
`(nested · extracted lexically)`.

### 8.14 Command steps in the chain

An external command is a step of the enrichment chain like any expression
step: it has a position, an id, and an output name (its **prefix**). Nothing
about where a command sits is special except what it may read.

- **One list, one order.** The Enrichment list (§12.5) shows command steps in
  place, as `⚙ <name> · <run state> · <program> <args>` rows, interleaved with
  expression rows. `Edit` on a command row opens the External command dialog
  on it; `External command…` opens the selected command step, or inserts a
  new one after the selection; `Remove` drops either kind. Alt-Up / Alt-Down
  move the selected step. Every one of these is a chain change through the
  query seam, accepted or rejected as a whole, and a rejection keeps the
  accepted chain and says why in the message row.
- **The order is the meaning.** A step reads a command's output as
  `<name>.<field>` columns, exactly as it reads an earlier expression's
  output. A step may only read a command that comes before it; the chain is
  rejected otherwise (`step X reads geo.city before command step geo runs;
  move the command step above it`). A command's input is the steps before it,
  and never its own or a later step's output.
- **Saving never runs.** Saving, reordering, restoring a view, cloning a view
  and applying a recipe never start a program (AGENTS.md). A run is explicit:
  review, then confirm, from the command's own dialog. The user's decision on
  whether a re-run could ever be automatic is recorded in
  `docs/command-chain.md`; this rule assumes it is not.
- **Valid before the run.** A filter, search or later step that reads an
  unrun command's output is valid: the columns are typed null, a step that
  cannot evaluate over null carries a diagnostic (`waits for a command step
  that has not run`) and reads as null, and a filter over them is not applied
  yet (the status says `filter waits for a command step that has not run`),
  because applying it would hide the very rows the command needs as its
  input. Once results are published the chain is re-evaluated over them as
  one accepted query, so the view changes exactly once and only then.
- **Stale is a word, not a guess (§7.4).** A command step is `Unrun` until
  its first publication and after its definition changes; the list's message
  row names every unrun step (`3 steps active · 1 unrun: geo`), the row says
  `unrun` or `results published`, and Details shows `<name>.status` as
  `Pending — run explicitly` or `Ready · last explicit run` per record. A
  changed definition keeps the last publication readable until a run
  replaces it; a step's results are dropped only when the step is removed.
- **Recipes carry the step, never its results.** A recipe stores a command
  step's program, arguments, working directory, environment and name. Applying
  it installs the step unrun. A recipe that names a program this machine
  cannot start says so when it is applied (`recipe applied · not on this
  machine: geo needs /opt/geo · the step is saved unrun`), not at the first
  run and not silently.
- **Names.** A prefix is an identifier of at most 64 characters, not `raw`
  and not `_lvu_*`, unique among the chain's command steps. The default is
  `command`, then `command2`, `command3`… so a chain that grew from the old
  single command keeps its `command.<field>` names.

## 9. Overflow

| Situation | Rule | Affordance |
| --- | --- | --- |
| Form or dialog taller than its class max | Body scrolls as one region | 1-column scrollbar in the body's last column; focused control kept visible |
| List longer than its viewport | Viewport windows on the selection | Count `n of m` in the heading + scrollbar |
| Read-only pane longer than its viewport | Pane scrolls (focusable) | Scrollbar |
| Multi-line input longer than its cap | Input scrolls internally | Scrollbar inside the input's last column |
| Long single-line value | Tail window with leading `…` | — |
| Long list cell | Truncate with trailing `…`; the rightmost column truncates first, never the identifier | — |
| Too many buttons | Wrap once, then `[ More ▾ ]` | Popup |
| Content fits | No affordance is drawn | — |

Never: `[ ▲ Scroll up ]`/`[ ▼ Scroll down ]` buttons, `↑/↓ scroll` text, counts
in pane titles that appear only after overflow, pagination, or dialogs that
change their information architecture at small sizes (`[ More ]` for fields).
Every scrollable region has a mouse hitbox equal to its rect.

---

## 10. Layering

- A **child dialog** (the step editor under Enrichment; Note under Bookmarks;
  cleanup confirmation under Storage) is a normal dialog of its own class,
  centred, drawn after a second scrim pass over the parent. The parent's border
  drops to `border` colour and its title stays visible so the stack is legible;
  the child's title is a breadcrumb (`Enrichment › Edit step`).
- External command and Recipes' History are *not* children, though their
  breadcrumb titles read like one. Neither draws its parent behind it and
  neither returns to it on Escape, so each replaces the layer it came from
  (component-model.md §6.5, steps 9 and 13).
- Child width ≤ parent width − 4 and child height ≤ parent height − 2 when the
  terminal is not `compact`; in `compact` terminals the child takes the parent's
  rect and the parent is not drawn (breadcrumb title keeps the context).
- Escape closes the frontmost layer only: anchored popup → child → parent.
- Anchored popups (dropdowns, completions) are not dialogs: no scrim, no
  breadcrumb, drawn last, `active_border` border.
- At most one child level. A workflow that would need a grandchild is a
  segmented mode or a pane instead.

---

## 11. Anti-patterns (each observed in the captures)

1. A bordered box inside a dialog for one line of text (`┌ Applied ┐`,
   `┌ State ┐`, `┌ Status ┐`).
2. Semantic colour on a box border (green `Applied`, muted `Proposal and activity`).
3. Help, warnings or data in a title (`— selection never auto-starts`,
   `— total 134.8 KiB`, `· runs only when confirmed`, `· Ctrl-P`).
4. Help in a pane heading (`Saved steps · kept when you add`).
5. Label and help on one row with the value on the next (`Program: Executable
   path; no shell parsing`).
6. ALL CAPS labels; inline `Label:` prefixes; status stutter (`Saved: Saved;`).
7. Buttons above the fields they act on; mode switchers rendered as buttons;
   a button row that changes order between modes (`[ Start reviewed ]`
   pushing `[ Manual ]` right).
8. Input background painted wider than the input, over several fields, or over
   a label.
9. Empty rows used to reach a fixed height; fixed heights at all.
10. Key-reminder footers (`↑/↓ scroll · g anchor`), scroll pseudo-buttons,
    unannounced windowed lists; a chord in a help sentence (`Alt-C adds`,
    `Escape cancels`); a Help entry for something a button already shows (§8.10).
11. Wrapping mid-word or continuing a wrapped row at column 0.
12. Clipping a value while keeping its help; clipping from the start (`xture/provider`).
13. Two widgets writing into the same cells.
14. Implementation jargon in ordinary controls (`Preview search="" advanced=false …`,
    `1 line(s)`, `Procfs Limited`).
15. Dangling separators (`1 / 128 bookmarks ·`).

---

## 12. Before / after by dialog

"Before" shows the dialog region of the real 100x30 capture. "After" mockups
are exact: widths are the §5.3 class widths at 100x30 (S 60, M 72, L 86,
XL 98, P 64) and 52 at 54x16; `▁` marks the caret cell; the shaded input rects
are the runs from the field column to the field's right edge. Blank rows inside
the mockups are the `pad`/`gap` tokens; at 54x16 they are 0 and help is dropped.

### 12.1 Search `/` — class S

Before (80% × 11; label-less invisible input; four blank rows; examples at the
bottom read as a footer):

```
┌ Search ──────────────────────────────────────────────────────────────────────┐
│                                                                              │
│ Applied  No filter applied.                                                  │
│                                                                              │
│                                                                              │
│                                                                              │
│                                                                              │
│ Examples: text · "field name": text · /regex/ims · \/literal                 │
│                                                                              │
│                                                                              │
└──────────────────────────────────────────────────────────────────────────────┘
```

After, 100x30 (60 × 8, top-biased so the filtered log stays visible):

```
┌ Search ──────────────────────────────────────────────────┐
│                                                          │
│  request 1▁                                              │
│                                                          │
│  ●  Applied   request 1 · 10 of 64 records match         │
│  text · "field": text · /regex/ims · \/literal           │
│                                                          │
└──────────────────────────────────────────────────────────┘
```

Empty state: placeholder `Type to filter…`; message `○  No filter  showing all
64 records`. While the query is being evaluated: `◐  Updating  request 1`.

After, 54x16 (52 × 5):

```
┌ Search ──────────────────────────────────────────┐
│  request 1▁                                      │
│  ●  Applied   request 1 · 10 of 64 match         │
└──────────────────────────────────────────────────┘
```

### 12.2 Advanced filter `p` — class S

Before: identical frame to Search with an `FILTER EXPRESSION` caps label and a
two-line help paragraph.

After, 100x30:

```
┌ Advanced filter ─────────────────────────────────────────┐
│                                                          │
│  Polars expression, e.g. col("level") == "ERROR"▁        │
│                                                          │
│  ○  No filter  showing all 64 records                    │
│  Fields and sampled literals complete with Tab           │
│                                                          │
└──────────────────────────────────────────────────────────┘
```

Error state replaces the message: `✖  Error     advanced Polars adapter is not
wired`. Completion popup is class A anchored under the field. 54x16 as Search.

### 12.3 Multiline grouping `m` — class S

Before: label row, invisible input, `Applied: Grouping disabled.`, two blank
rows, `Preview (display only):`, three preview lines, `Empty draft disables
grouping`.

After, 100x30 (60 × 11):

```
┌ Multiline grouping ──────────────────────────────────────┐
│                                                          │
│  ^(\s+|Caused by:)▁                                      │
│                                                          │
│  Preview                                                 │
│    RuntimeException: boom                                │
│      at worker.rs:42                          2 lines    │
│                                                          │
│  ○  Disabled  an empty draft turns grouping off          │
│  Continuation lines match this regex over raw bytes;     │
│  grouping is display only                                │
│                                                          │
└──────────────────────────────────────────────────────────┘
```

54x16: preview pane capped at 2 rows, help dropped, no pads (52 × 8).

### 12.4 Time window `t` — class M

Before: see §1.

After, 100x30 (72 × 15):

```
┌ Time window ─────────────────────────────────────────────────────────┐
│                                                                      │
│  Time basis   Capture                        ▾                       │
│  Window       All time                       ▾                       │
│  Gap jump     Quiet ≥ 1m                     ▾                       │
│                                                                      │
│  Start        2026-09-07  02:12:44.343236213  UTC       ▾            │
│  End          2026-09-07  02:13:44.343236213  UTC       ▾            │
│                                                                      │
│  ●  Applied   all times                                              │
│  Bounds are half-open. UTC and numeric offsets only; named zones     │
│  are not supported.                                                  │
│                                                                      │
│  [ Apply ]  [ Clear ]  [ 🧠 Recognize timestamp ]                    │
│                                                                      │
└──────────────────────────────────────────────────────────────────────┘
```

Field column at label_w 10 + gutter. Dropdowns are 32 wide (longest option
`± 30s around selected` + 4, rounded to match the date+time run); date 10, time
18, zone 10 with `gutter` between. Start/End are disabled (muted) unless Window
is `Absolute`, and the message explains it while they are focused.

`Gap jump` is the quiet period `{`/`}` navigate to. It lives here because a gap
is a fact about time, and because a threshold the user cannot see is one they
cannot trust: the row states the number, the dropdown changes it, and the status
line names it again when a jump finds nothing.

Open Window dropdown (class A, anchored under the field, 32 wide):

```
│  Window       All time                       ▾                       │
│               ┌──────────────────────────────┐                       │
│  Start        │ All time                     │                       │
│  End          │ Absolute                     │                       │
│               │ Last 5m by clock             │                       │
│  ●  Applied   │ Last 15m by clock            │                       │
│  Bounds are h │ Last 1h by clock             │named zones            │
│  are not supp │ First → last event           │                       │
│               │ Last 5m of data              │                       │
│               │ Last 1h of data              │                       │
│               │ ± 30s around selected        │                       │
│               │ ± 5m around selected         │                       │
│               └──────────────────────────────┘                       │
```

The clock-relative and data-relative windows are named apart on purpose. "Last
5 minutes" means two different things depending on whether the stream is live
or was captured yesterday, and a label that does not say which is an inference
the user has to make. `by clock` stays a rolling policy that ages rows out even
when nothing arrives; `of data` and `First → last event` resolve once against
the view's own first and last record and write the result into Start/End, where
it can be seen and narrowed. The `±` width is part of the choice rather than a
hidden constant, for the same reason.

Rows measured against the data appear only when the view can report its first
and last record in the basis it is filtered on. Offering a choice that would
silently do nothing is worse than not offering it.

After, 54x16 (52 × 12; Start/End reflow to one field per row; help and pads
dropped):

```
┌ Time window ─────────────────────────────────────┐
│  Time basis   Capture                     ▾      │
│  Window       All time                    ▾      │
│  Start date   2026-09-07                         │
│  Start time   02:12:44.343236213                 │
│  Start zone   UTC       ▾                        │
│  End date     2026-09-07                         │
│  End time     02:13:44.343236213                 │
│  End zone     UTC       ▾                        │
│  ●  Applied   all times                          │
│  [ Apply ]  [ Clear ]  [ 🧠 Recognize timestamp ]│
└──────────────────────────────────────────────────┘
```

### 12.5 Enrichment `e` — class L

Before (six buttons on top mixing views and actions; three nested boxes; help
in box titles; `Status` heading over an `Applied:` line):

```
┌ Enrichment ──────────────────────────────────────────────────────────────────┐
│ [ Steps ] [ Editor ] [ Add ] [ Edit ] [ Remove ] [ External command ]        │
│                                                                              │
│ ┌ Saved steps · kept when you add ─────────────────────────────────────────┐ │
│ │No extracted fields yet. Add an expression below.                         │ │
│ └──────────────────────────────────────────────────────────────────────────┘ │
│ ┌ Add step · name = expression OR /regex with named groups/ ───────────────┐ │
│ │                                                                          │ │
│ │                                                                          │ │
│ └──────────────────────────────────────────────────────────────────────────┘ │
│ Status                                                                       │
│ Applied: Accepted steps are active; a new draft changes nothing until it     │
│ succeeds.                                                                    │
│ ┌ Raw input before enrichment: ──────┐┌ Accepted output · same record ─────┐ │
│ │{"timestamp":                       ││No accepted outputs yet.            │ │
│ └────────────────────────────────────┘└────────────────────────────────────┘ │
└──────────────────────────────────────────────────────────────────────────────┘
```

After, 100x30 (86 × 19). One layout; steps list is a pane; the add field is a
labelled row; raw/output are side-by-side panes (content width 82 ≥ 72):

```
┌ Enrichment ────────────────────────────────────────────────────────────────────────┐
│                                                                                    │
│  Steps                                                                  2 of 2     │
│    › 1  level      json.level                                        applied       │
│      2  latency    /completed in (?P<ms>\d+)ms/                      applied       │
│                                                                                    │
│  Add step     name = expression  or  /regex with (?P<name>…) groups/▁              │
│                                                                                    │
│  Raw input                                Accepted output                          │
│    {"timestamp": "2026-09-06T12:00:19…      level     DEBUG                        │
│    "level": "DEBUG", "service": "work…      latency   57                           │
│    "req-0019", "message": "fixture re…                                             │
│                                                                                    │
│  ●  Applied   2 steps active · a draft changes nothing until it succeeds           │
│                                                                                    │
│  [ Add ]  [ Edit ]  [ Remove ]  [ External command… ]                              │
│                                                                                    │
└────────────────────────────────────────────────────────────────────────────────────┘
```

Empty state: the Steps pane shows one row `No steps yet · add an expression
below`; Accepted output shows `No accepted outputs yet`. `Edit` loads the
selected step into the field and the primary button reads `Save` until the
draft is committed or cleared.

Command steps (§8.14) are rows of the same list, in chain order, and the
`External command` summary pane is gone:

```
│  Steps                                                                  3 of 3     │
│      1  /"request":"(?P<req>r-\d+)"/                                                │
│      2  ⚙ geo · unrun · /opt/geo --json                                             │
│    › 3  hot = pl.col('geo.tier') == 'hot'                                           │
│                                                                                    │
│  ◐  Pending   3 steps active · 1 unrun: geo                                        │
│  Later steps can use fields from earlier steps, command output as <name>.<field>   │
│  · Alt-Up/Down reorder · commands run only when you confirm                        │
```

`⚙` (`$` in ASCII) marks a command row; the run state after the name is
`unrun` or `results published`, before the program so a long path never
hides it. `Edit` on a command row opens §12.6 on that
step; `External command…` opens the selected command step or inserts a new
one after the selection; `Remove` drops either kind; Alt-Up / Alt-Down move
the selected step and the selection follows it. With every command step
published the message row is `●  Applied   3 steps active`.

Default action (§8.9): `Add` while the chain is empty, `Edit` once a step is
selected — the fill moves between the two buttons and Enter on the Steps list
runs whichever is current. The list opens on the step the user last selected,
clamped to the chain, so the first frame always has a real row under `›`.

After, 54x16 (52 × 16; panes stack, each capped at 3 rows; body scrolls with a
scrollbar in column 50):

```
┌ Enrichment ─────────────────────────────────────┐
│  Steps                                  2 of 2 ▲│
│    › 1  level     json.level          applied  █│
│      2  latency   /completed in (?P<…  applied  │
│  Add step                                       │
│  name = expression or /regex…▁                  │
│  Raw input                                      │
│    {"timestamp": "2026-09-06T12:00:19.019000Z", │
│    "level": "DEBUG", "service": "worker", "requ │
│  Accepted output                                │
│    level     DEBUG                              │
│    latency   57                                ▼│
│  ●  Applied   2 steps active                    │
│  [ Add ]  [ Edit ]  [ Remove ]                  │
│  [ External command… ]                          │
└─────────────────────────────────────────────────┘
```

### 12.6 External command (Enrichment child) — class L child

Before: label+help rows alternating with invisible value rows; `Applied
command step:` between fields and buttons; a `Status and review` box with five
empty rows; warning in the title.

After, 100x30 (82 × 19, i.e. parent width − 4, over the scrimmed Enrichment):

```
┌ Enrichment › External command ───────────────────────────────────────────────────┐
│                                                                                  │
│  Name          geo                                                               │
│  Program       /usr/bin/jq▁                                                      │
│  Arguments     -c                                                                │
│                .                                                                 │
│  Directory     (workspace directory)                                             │
│  Environment   TZ=UTC                                                            │
│                                                                                  │
│  Results and review                                                    3 of 6    │
│    Applied command step geo: /usr/bin/jq (2 arguments) · step 2 of 3;       ▲    │
│    later steps may read geo.<field>                                         █    │
│    Saving or restoring never starts this command.                           ▼    │
│                                                                                  │
│  ○  Unrun     saved definition · not run · runs only when you confirm            │
│  Program is an executable path; no shell parsing. One argument per line.         │
│                                                                                  │
│  [ Save ]  [ Review and run ]  [ Remove ]  [ New line ]                          │
│                                                                                  │
└──────────────────────────────────────────────────────────────────────────────────┘
```

`Name` is the step's output prefix (§8.14): results appear as
`<name>.<field>` in Details and to later steps, and `<name>.status` says
`Ready` or `Pending`. Its placeholder is `(output prefix)`; a new step opens on
the first free default (`command`, `command2`…), and a name that is not a
valid, unique prefix is an `Error` on save. The dialog opens with the caret in
`Program`; Shift-Tab reaches `Name`. The notes say where the step sits
(`step 2 of 3`) or where a new one will go (`will be inserted as step 2 of
3`). `Arguments` and `Environment` are multi-line inputs (3 visible rows max,
the painted rect grows with lines up to the cap). `(workspace directory)` is
the placeholder for an empty Directory. `New line` is the last, non-primary
button.

Save is a chain change: the dialog shows `Saving definition…` while the
query seam checks the chain, then `Unrun` with `Saved definition · not run`,
or `Error` with the seam's reason (a step that reads this command from above
it, a name in use). A save never runs the program; the review and its
confirmation are the only way to.

After, 54x16 (52 × 16, replaces the parent in place):

```
┌ Enrichment › External command ──────────────────┐
│  Name          geo                             ▲│
│  Program       /usr/bin/jq▁                    █│
│  Arguments     -c                               │
│                .                                │
│  Directory     (workspace directory)            │
│  Environment   TZ=UTC                           │
│  Results and review                     3 of 6  │
│    Saving or restoring never starts this comma… │
│    New records stay pending until you run it a… │
│    Results appear in Details as command.<field… │
│                                                ▼│
│  ○  Unrun     runs only when you confirm        │
│  [ Save ]  [ Review and run ]  [ Remove ]       │
│  [ New line ]                                   │
└─────────────────────────────────────────────────┘
```

### 12.7 Add source `n` — class L, three modes

Before (manual; 100% × 24; 16 empty rows; caps label; boxed state; two mode
switchers in the button row):

```
┌ Add source ──────────────────────────────────────────────────────────────────────────────────────┐
│ FILE PATH                                                                                        │
│                                                                                                  │
│                                  … 14 more empty rows …                                          │
│ ┌ State ───────────────────────────────────────────────────────────────────────────────────────┐ │
│ │Ready: provide a file path or command. Capture starts only after submission.                  │ │
│ └──────────────────────────────────────────────────────────────────────────────────────────────┘ │
│                                                                                                  │
│ [ Manual ] [ Discover ] [ 🧠 ] [ File ] [ Command ]                                              │
└──────────────────────────────────────────────────────────────────────────────────────────────────┘
```

After — Manual, 100x30 (86 × 14 with two suggestions; 86 × 11 with none):

```
┌ Add source ────────────────────────────────────────────────────────────────────────┐
│                                                                                    │
│   Manual │ Discover │ 🧠 Agent                                                     │
│                                                                                    │
│  Kind         ● File   ○ Command                                                   │
│  Path         /var/log/app▁                                                        │
│                                                                                    │
│  Suggestions                                                            2 matches  │
│    › /var/log/app.log                                                              │
│      /var/log/app.log.1                                                            │
│                                                                                    │
│  ○  Ready     capture starts only when you open the source                         │
│                                                                                    │
│  [ Open ]                                                                          │
│                                                                                    │
└────────────────────────────────────────────────────────────────────────────────────┘
```

Kind `Command` relabels the field `Command` and the placeholder
`program and arguments, e.g. journalctl -f`. Enter on a suggestion opens it
directly, as today; `[ Open ]` is the same action for the typed value.

After — Discover, 100x30 (86 × 17):

```
┌ Add source ────────────────────────────────────────────────────────────────────────┐
│                                                                                    │
│   Manual │ Discover │ 🧠 Agent                                                     │
│                                                                                    │
│  Filter       ▁                                                                    │
│                                                                                    │
│  Candidates                                                               2 of 2   │
│    › events.log             remembered    unknown                                  │
│      events.log             project       medium · available                       │
│                                                                                    │
│  Details                                                                           │
│    /tmp/lvu-design-cap/events.log · remembered source                              │
│    Scan complete · 1 candidate · process scan limited by the file descriptor cap   │
│                                                                                    │
│  ○  Ready     selecting a candidate never starts capture                           │
│                                                                                    │
│  [ Open ]  [ Rescan ]                                                              │
│                                                                                    │
└────────────────────────────────────────────────────────────────────────────────────┘
```

While scanning: `◐  Updating  scanning · 1 candidate so far`. Candidate columns:
name (fill), origin (12), confidence · availability (fill).

After — 🧠 Agent, 100x30 (86 × 15 before a proposal):

```
┌ Add source ────────────────────────────────────────────────────────────────────────┐
│                                                                                    │
│   Manual │ Discover │ 🧠 Agent                                                     │
│                                                                                    │
│  Describe     Follow the nginx access log for the api container▁                   │
│               ▁                                                                    │
│                                                                                    │
│  Proposal                                                                none yet  │
│    A reviewed source definition appears here; nothing runs until you start it.     │
│                                                                                    │
│  ○  Ready     describe the source to follow                                        │
│                                                                                    │
│  [ Request proposal ]                                                              │
│                                                                                    │
└────────────────────────────────────────────────────────────────────────────────────┘
```

With a proposal, the Proposal pane lists `Name`, `Command`, `Directory`,
`Environment`, `Restart` and the explanation as label/value rows (scrollable),
the message becomes `●  Ready     proposal received · review before starting`,
and the actions become `[ Start reviewed source ]  [ Request again ]`. The
segmented control never moves.

After, 54x16 (Manual; 52 × 10):

```
┌ Add source ─────────────────────────────────────┐
│   Manual │ Discover │ 🧠 Agent                  │
│  Kind      ● File   ○ Command                   │
│  Path      /var/log/app▁                        │
│  Suggestions                          2 matches │
│    › /var/log/app.log                           │
│      /var/log/app.log.1                         │
│  ○  Ready     opens only when you choose        │
│  [ Open ]                                       │
└─────────────────────────────────────────────────┘
```

### 12.8 View `v` — class M

Before (mode in caps; help truncated; five equal buttons flush against the
border; at 54x16 the buttons overprint the help):

```
┌ Source view ─────────────────────────────────────────────────────────┐
│ Mode: CLONE SETTINGS                                                 │
│                                                                      │
│ Name: Copy of Raw events                                             │
│                                                                      │
│ Name the view. Creating, cloning, and renaming preserve the source   │
│[ New blank ] [ Clone ] [ Rename ] [ Sources ] [ Apply ]              │
└──────────────────────────────────────────────────────────────────────┘
```

After, 100x30 (72 × 12). One dialog for the current view: rename by editing
the name, change membership with the checkboxes, `Apply` commits both.
`Clone` and `New blank view` create and switch:

```
┌ View · Raw events ───────────────────────────────────────────────────┐
│                                                                      │
│  Name         Raw events▁                                            │
│                                                                      │
│  Sources                                                    1 of 1   │
│    [x] events.log                   running · 64 records             │
│                                                                      │
│  ○  Ready     renaming or changing sources keeps the capture         │
│                                                                      │
│  [ Apply ]  [ Clone ]  [ New blank view ]                            │
│                                                                      │
└──────────────────────────────────────────────────────────────────────┘
```

Alt-Up/Alt-Down reorder within the Sources list as today; the order note
(`source position, then record sequence`) becomes the help row when there are
two or more sources.

After, 54x16 (52 × 7):

```
┌ View · Raw events ──────────────────────────────┐
│  Name      Raw events▁                          │
│  Sources                                 1 of 1 │
│    [x] events.log        running · 64 records   │
│  ○  Ready     changes keep the capture          │
│  [ Apply ]  [ Clone ]  [ New blank view ]       │
└─────────────────────────────────────────────────┘
```

### 12.9 Recipes `r` — class M

Before: `(no saved recipes)` / `Applied: 0 saved recipes` at the top, twelve
blank rows, eight equal buttons on two rows flush against the border.

After, 100x30 (72 × 13 with the empty list; grows one row per recipe to 12):

```
┌ Recipes ─────────────────────────────────────────────────────────────┐
│                                                                      │
│  Saved recipes                                              0 of 0   │
│    No saved recipes yet · Save stores the current filters            │
│                                                                      │
│  Name         ▁                                                      │
│                                                                      │
│  ○  Ready     Apply restores a recipe's filters and enrichments      │
│                                                                      │
│  [ Apply ]  [ Save ]  [ Update ]  [ History ]  [ More ▾ ]            │
│                                                                      │
└──────────────────────────────────────────────────────────────────────┘
```

Populated rows: `› error triage          search "ERROR" · 2 enrichments   2026-09-06`
(name 22, summary fill, date 10). The date is when that revision was saved, in
the app's display zone, right-aligned; a recipe written before the stored
document carried a date shows `—` rather than a guess, and History dates each
revision the same way. The date only takes its column when a readable summary
still fits beside it (§4.4); the revision id it replaced is still what the
message row, History and Export name, so the identity the user acts on is not
lost. `More ▾` opens `Import…`, `Export…`,
`Refresh`. `History` opens a child M list `Recipes › error triage history`
whose primary button is `Apply revision`. The implementation-shaped preview line
(`Preview search="" advanced=false …`) is replaced by the summary column.

After, 54x16 (52 × 8):

```
┌ Recipes ────────────────────────────────────────┐
│  Saved recipes                           0 of 0 │
│    No saved recipes yet                         │
│  Name      ▁                                    │
│  ○  Ready     Apply restores a recipe           │
│  [ Apply ]  [ Save ]  [ Update ]  [ History ]   │
│  [ More ▾ ]                                     │
└─────────────────────────────────────────────────┘
```

### 12.10 Bookmarks `B` — class M

Before: 100% × 22; `1 / 128 bookmarks ·`; one row; sixteen blank rows;
buttons indented one column and `↑/↓ select` flush left beneath them.

After, 100x30 (72 × 11 with one bookmark):

```
┌ Bookmarks · Raw events ──────────────────────────────────────────────┐
│                                                                      │
│  Bookmarks                                                  1 of 1   │
│    › #19   02:13:14   fixture request 19 completed in 57ms           │
│            no note                                                   │
│                                                                      │
│  ○  Ready     Enter jumps to the selected bookmark                   │
│                                                                      │
│  [ Go to ]  [ Edit note ]  [ Raw context ]  [ Remove ]               │
│                                                                      │
└──────────────────────────────────────────────────────────────────────┘
```

Rows are two lines: id (5), time (8), record text (fill); second line the note
or `no note` in `muted` only when absent (it is not information). `Edit note`
opens an S child `Bookmarks › Note for #19` with one fill field and `[ Save ]`.

After, 54x16 (52 × 7):

```
┌ Bookmarks · Raw events ─────────────────────────┐
│  Bookmarks                               1 of 1 │
│    › #19  02:13:14  fixture request 19 complet… │
│           no note                               │
│  ○  Ready     Enter jumps to the bookmark       │
│  [ Go to ]  [ Edit note ]  [ Raw context ]      │
│  [ Remove ]                                     │
└─────────────────────────────────────────────────┘
```

### 12.11 Fields `i` — class L

Before: 70% × 16; `> [ ] level = DEBUG` rows; key-list footer.

Now (§8.11–§8.12), 100x30 (86 × 19): the list is a tree with a Value pane
beside it, and six one-key actions on the selected value:

```
┌ Fields · record 19 ────────────────────────────────────────────────────────────────┐
│                                                                                    │
│  Field         Value               6 fields    Value · http.status  first 2,048 records
│    › [ ] level        "INFO"                     Type       integer · 100% of present values
│      [ ] message      "request 19 done"          Sample     200 · record 19
│       ▸  http         {3 keys}                   Present    2,048 of 2,048 sampled records
│      [ ] request_id   "req-0019"                 Distinct   4 values
│      [ ] service      "worker"                   Range      200 … 503
│      [ ] timestamp    "2026-09-06T12:00:19Z"     Top         1,203  200
│                                                                512  404
│                                                                                    │
│  Pinned fields become log columns; a nested value acts through its top-level field.│
│                                                                                    │
│  [ Pin ]  [ Filter ]  [ Exclude ]  [ Color ]  [ Fold ]  [ Correlate ]              │
│                                                                                    │
└────────────────────────────────────────────────────────────────────────────────────┘
```

Opening `http` (Enter or Right on its row) inserts `status`, `path` and
`tags [2]` beneath it, indented, and the checkbox column is blank for nested
rows because pinning acts on the top-level column. Below 72 columns of
content the panes stack (list capped at 8 rows, then the 8-line Value pane).
The earlier class-M mockup follows for the flat, non-JSON case, which keeps
its shape.

Before (§12.11 as first specified), 100x30 (72 × 14):

```
┌ Fields · record 19 ──────────────────────────────────────────────────┐
│                                                                      │
│  Field           Value                                    6 fields   │
│    › [ ] level        DEBUG                                          │
│      [ ] message      fixture request 19 completed in 57ms           │
│      [ ] path         /v1/items/19                                   │
│      [ ] request_id   req-0019                                       │
│      [ ] service      worker                                         │
│      [ ] timestamp    2026-09-06T12:00:19.019000Z                    │
│                                                                      │
│  Pinned fields become log columns                                    │
│                                                                      │
│  [ Pin ]  [ Color rows by field ]                                    │
│                                                                      │
└──────────────────────────────────────────────────────────────────────┘
```

No message row (no state). Space toggles the pin on the selected row (§8.4);
`[ Pin ]` reads `[ Unpin ]` when the selected field is pinned. Empty state: one
row `No fields for this record`.

After, 54x16 (52 × 11):

```
┌ Fields · record 19 ─────────────────────────────┐
│  Field         Value                   6 fields │
│    › [ ] level       DEBUG                      │
│      [ ] message     fixture request 19 compl…  │
│      [ ] path        /v1/items/19               │
│      [ ] request_id  req-0019                   │
│      [ ] service     worker                     │
│      [ ] timestamp   2026-09-06T12:00:19.0190…  │
│  [ Pin ]  [ Color rows by field ]               │
└─────────────────────────────────────────────────┘
```

### 12.12 Raw context `o` — class XL

Before (kept largely as is; the user likes its use of space):

```
┌ Raw context · filter unchanged ──────────────────────────────────────────────────────────────────┐
│ Anchor: ed4a0c76-63b7-59e8-bbbd-5167f7c3ec5c:19 · physical source records                        │
│ 15–35 / 64 · raw, unfiltered, ungrouped                                                          │
│       14 {"timestamp": "2026-09-06T12:00:14.014000Z", "level": "ERROR", "service": "scheduler",  │
│ >     19 {"timestamp": "2026-09-06T12:00:19.019000Z", "level": "DEBUG", "service": "worker", "re │
│↑/↓ scroll · g anchor                                                                             │
└──────────────────────────────────────────────────────────────────────────────────────────────────┘
```

After, 100x30 (98 × 28). Changes: one header line, list gutter + scrollbar,
message row instead of `filter unchanged` in the title, a button instead of a
key list:

```
┌ Raw context · Raw events ──────────────────────────────────────────────────────────────────────┐
│                                                                                                │
│  Anchor #19 · ed4a0c76…:19 · records 15–35 of 64 · raw, unfiltered, ungrouped                  │
│                                                                                                │
│      14  {"timestamp": "2026-09-06T12:00:14.014000Z", "level": "ERROR", "service": "schedul… ▲ │
│      15  {"timestamp": "2026-09-06T12:00:15.015000Z", "level": "DEBUG", "service": "api", "… █ │
│      …                                                                                       █ │
│    › 19  {"timestamp": "2026-09-06T12:00:19.019000Z", "level": "DEBUG", "service": "worker"… █ │
│      …                                                                                         │
│      34  {"timestamp": "2026-09-06T12:00:34.034000Z", "level": "ERROR", "service": "worker"… ▼ │
│                                                                                                │
│  ○  Unfiltered  the accepted filter still applies to the log behind this dialog                │
│                                                                                                │
│  [ Back to anchor ]                                                                            │
│                                                                                                │
└────────────────────────────────────────────────────────────────────────────────────────────────┘
```

The body takes every remaining row (21 record rows at 100x30, 33 at 140x40).
`g` remains the accelerator for `Back to anchor`; it is not printed.

After, 54x16 (52 × 16; header wraps to one truncated line, message shortened):

```
┌ Raw context · Raw events ───────────────────────┐
│  Anchor #19 · records 15–25 of 64 · raw         │
│      14  {"timestamp": "2026-09-06T12:00:14.0… ▲│
│      15  {"timestamp": "2026-09-06T12:00:15.0… █│
│      16  {"timestamp": "2026-09-06T12:00:16.0… █│
│      17  {"timestamp": "2026-09-06T12:00:17.0… █│
│      18  {"timestamp": "2026-09-06T12:00:18.0… █│
│    › 19  {"timestamp": "2026-09-06T12:00:19.0… █│
│      20  {"timestamp": "2026-09-06T12:00:20.0…  │
│      21  {"timestamp": "2026-09-06T12:00:21.0…  │
│      22  {"timestamp": "2026-09-06T12:00:22.0…  │
│      23  {"timestamp": "2026-09-06T12:00:23.0…  │
│      24  {"timestamp": "2026-09-06T12:00:24.0… ▼│
│  ○  Unfiltered  filter unchanged behind         │
│  [ Back to anchor ]                             │
└─────────────────────────────────────────────────┘
```

### 12.13 Storage `S` — class L

Before: data in the title; three unlabelled summary lines; run-on entry rows;
boxed `Status`; key-list footer.

```
┌ Storage usage — total 134.8 KiB / unused derived 0 B ────────────────────────────────┐
│ row cache 28.5 KiB / 4.0 MiB   query membership 208 B / 256.0 MiB                    │
│ derived disk cap/source 256.0 MiB · global 5.0 GiB                                   │
│ managed budgets; not a process RSS limit                                             │
│                                                                                      │
│ > derived        32 B .lvu-index-budget — unrecognized or symlink; preserved         │
│   derived     2.6 KiB ed4a0c76-63b7-59e8-bbbd-5167f7c3ec5c.d17625e2-232f-45e5-affc-1 │
│ ┌ Status ──────────────────────────────────────────────────────────────────────────┐ │
│ │Status: scan complete; c previews cleanup, c again confirms                       │ │
│ └──────────────────────────────────────────────────────────────────────────────────┘ │
│↑/↓ active pane · r refresh · c preview/confirm cleanup                               │
└──────────────────────────────────────────────────────────────────────────────────────┘
```

After, 100x30 (86 × 17 with five entries):

```
┌ Storage ───────────────────────────────────────────────────────────────────────────┐
│                                                                                    │
│  Row cache      28.5 KiB of 4.0 MiB       Membership     208 B of 256.0 MiB        │
│  Derived disk   134.8 KiB of 5.0 GiB      Per source     256.0 MiB cap             │
│                                                                                    │
│  Entries                                                   5 of 5 · 0 B reclaimable│
│    › derived       32 B     .lvu-index-budget              unrecognized · kept     │
│      derived     2.6 KiB    ed4a0c76…rows.idx                active · locked       │
│      derived       0 B     .lvu-index-ownership.lock       unrecognized · kept     │
│      capture    32.2 KiB    source ed4a0c76…               journal · kept          │
│      workspace  100.0 KiB   memory + recipes               durable · kept          │
│                                                                                    │
│  ●  Scanned   managed budgets only · not a process RSS limit                       │
│                                                                                    │
│  [ Refresh ]  [ Preview cleanup ]                                                  │
│                                                                                    │
└────────────────────────────────────────────────────────────────────────────────────┘
```

Entry columns: kind (10), size (10, right-aligned), name (fill, `…` in the
middle of long ids so both ends survive), status (22). After `Preview cleanup`
the message reads `◐  Pending   cleanup would remove 3 entries · 1.0 MiB` and
the button becomes the destructive `[ Confirm cleanup ]`. Errors use the error
state and stay until the next scan.
`Refresh` is the default (§8.9) and stays it while `Confirm cleanup` is
showing: the entry rows have no row action, so Enter rescans, and the
destructive step is only ever reached by pressing `c` twice or clicking.

After, 54x16 (52 × 14; summary stacks; entry status column dropped first):

```
┌ Storage ────────────────────────────────────────┐
│  Row cache      28.5 KiB of 4.0 MiB             │
│  Membership     208 B of 256.0 MiB              │
│  Derived disk   134.8 KiB of 5.0 GiB            │
│  Per source     256.0 MiB cap                   │
│  Entries                       5 of 5 · 0 B     │
│    › derived      32 B   .lvu-index-budget      │
│      derived    2.6 KiB  ed4a0c76…rows.idx      │
│      derived       0 B   .lvu-index-ownership.… │
│      capture   32.2 KiB  source ed4a0c76…       │
│      workspace 100.0 KiB memory + recipes       │
│  ●  Scanned   managed budgets only              │
│  [ Refresh ]  [ Preview cleanup ]               │
└─────────────────────────────────────────────────┘
```

### 12.14 Settings `,` — class L

Before: 100% × 30 covering the whole screen; `Provider/model: xture/provider`
clipped at the start; toggles as buttons; `[ Save ]` between fields and two
boxes; `Saved: Saved;`; at 54x16 a one-field-at-a-time `[ More ]` form.

After, 100x30 (86 × 27):

```
┌ Settings ──────────────────────────────────────────────────────────────────────────┐
│                                                                                    │
│  🧠 Agent                                                                          │
│  Provider / model   fixture/provider▁                                              │
│  Mode               full-access                     ▾                              │
│  Thinking           medium                          ▾                              │
│                                                                                    │
│  Appearance                                                                        │
│  Theme              love-dark                       ▾                              │
│  [ ] Delight    [x] Reduced motion    [ ] ASCII                                    │
│                                                                                    │
│  Cache limits (MiB)                                                                │
│  Rows               4          Membership   256                                    │
│  Derived total      5120       Per source   256                                    │
│                                                                                    │
│  Effective values and paths                                             1 of 8  ▲  │
│    Agent        fixture/provider · full-access · medium        settings.toml    █  │
│    Appearance   love-dark · delight off (LVU_NO_DELIGHT) · motion on · ASCII o… ▼  │
│                                                                                    │
│  ●  Saved     appearance applies now · cache limits apply after restart            │
│                                                                                    │
│  [ Save ]                                                                          │
│                                                                                    │
└────────────────────────────────────────────────────────────────────────────────────┘
```

Field column at label_w 17. Number fields are 8 wide; the second pair
(`Membership`, `Per source`) starts at field column + 8 + gutter with its own
label_w 12. The effective-values pane is a scrollable label/value list (8 rows:
agent, appearance, startup MiB, settings path, data path, cache path, capture
path, restart note) with a 2-row viewport at this height, growing when the
terminal is taller. A pending draft shows `◐  Pending   changes are not saved`.

After, 54x16 (52 × 16; same fields, body scrolls; nothing hidden behind `More`):

```
┌ Settings ───────────────────────────────────────┐
│  🧠 Agent                                      ▲│
│  Provider / model                              █│
│  fixture/provider▁                              │
│  Mode               full-access           ▾     │
│  Thinking           medium                ▾     │
│  Appearance                                     │
│  Theme              love-dark             ▾     │
│  [ ] Delight  [x] Reduced motion  [ ] ASCII     │
│  Cache limits (MiB)                             │
│  Rows               4                           │
│  Membership         256                         │
│  Derived total      5120                        │
│  Per source         256                        ▼│
│  ●  Saved     cache limits apply after restart  │
│  [ Save ]                                       │
└─────────────────────────────────────────────────┘
```

(`Provider / model` stacks its label because `content.width < label_w + 2 + 20`
is false here only for the fill field; the rule stacks it since a 17-wide label
leaves under 20 cells for a path-like value.)

### 12.15 Help `?` — class L

Before is already close. Changes: two columns when `content.width ≥ 88`
(not `body.width ≥ 92`, which excluded the 100-column terminal); wrapped
descriptions continue at the description column; a scrollbar in the last body
column; no footer (nothing to act on; `?` and Escape close). Section headings
keep their bold caps as today's one deliberate exception to sentence case,
because they are an index, not labels.

After, 100x30 (86 × 28, one column at content width 82):

```
┌ Help ──────────────────────────────────────────────────────────────────────────────┐
│                                                                                    │
│  EVERYWHERE                                                                      ▲ │
│    Ctrl-P                Open the command palette                                █ │
│    ?                     Open or close this help                                 █ │
│    Ctrl-L                Redraw the terminal                                     █ │
│    ,                     Open settings                                             │
│    q / Ctrl-C            Quit                                                      │
│                                                                                    │
│  LOGS & VIEWS                                                                      │
│    g / G                 Jump to first / last record                               │
│    ←/→ · 0               Pan the selected event / reset pan                        │
│    Alt-C in Enrichment   Add, edit, remove, or explicitly run the terminal         │
│                          command step                                              │
│    …                                                                             ▼ │
│                                                                                    │
└────────────────────────────────────────────────────────────────────────────────────┘
```

### 12.16 Command palette Ctrl-P — class P

Before: `Command palette · Ctrl-P` title; `>` prompt; three columns with
inconsistent category casing; a two-line detail block with no separation.

After, 100x30 (64 × 18 with 12 visible commands):

```
┌ Command palette ─────────────────────────────────────────────┐
│                                                              │
│  Type a command…▁                                            │
│                                                              │
│    › Add source                     n        Sources      ▲  │
│      Advanced filter                p        Filter       █  │
│      Ask 🧠                         A        Agent        █  │
│      Bookmarks and notes            B        View            │
│      Enrichment                     e        Filter          │
│      Expand or collapse group                View            │
│      Fields                         i        Fields          │
│      Follow new records             f        View            │
│      Grouping                       m        Filter          │
│      Help                           ?        Application     │
│      Investigation 🧠               I        Agent           │
│      Literal filter                 /        Filter       ▼  │
│                                                              │
│  Add source · open the source dialog                         │
│                                                              │
└──────────────────────────────────────────────────────────────┘
```

Columns: name (fill), shortcut (8), category (12, Title case, one vocabulary:
`Sources`, `Views`, `Filter`, `Fields`, `Agent`, `Application`). The last row
is the selected command's description in message-row position, `base_fg`, no
glyph. A disabled command stays in the list in `muted` italic and its
description explains why.

54x16: 52 × 16, 10 visible rows, same structure.

### 12.17 Ask 🧠 `A` — class L

Before: `[ Kind: Filter ▾ ] [ Submit ]` above the request; three boxes
(`Request`, `State`, `Proposal and activity`) with provider trivia in the last.

After, 100x30 (86 × 17 before a proposal):

```
┌ Ask 🧠 ────────────────────────────────────────────────────────────────────────────┐
│                                                                                    │
│  Kind         Filter                      ▾                                        │
│  Request      Show only errors from the worker service▁                            │
│               ▁                                                                    │
│               ▁                                                                    │
│                                                                                    │
│  Proposal                                                               none yet   │
│    A proposal appears here for review; nothing is applied until you accept it.     │
│                                                                                    │
│  Activity                                                                          │
│    fixture/provider · full-access · thinking medium                                │
│                                                                                    │
│  ○  Ready     describe the desired filter                                          │
│                                                                                    │
│  [ Submit ]                                                                        │
│                                                                                    │
└────────────────────────────────────────────────────────────────────────────────────┘
```

Request is a 3-row multi-line input (scrolls internally beyond 3). With a
proposal: the Proposal pane shows the proposed expression/steps and
explanation (scrollable), Activity shows the session log, the message becomes
`●  Ready     proposal received · review before applying`, actions become
`[ Accept ]  [ Reject ]  [ Submit again ]`. While waiting: `◐  Pending
waiting for the agent` and `[ Cancel request ]` as the only button (a real
action, not a dismissal).

After, 54x16 (52 × 13):

```
┌ Ask 🧠 ─────────────────────────────────────────┐
│  Kind       Filter                    ▾         │
│  Request    Show only errors from the worker s… │
│             ▁                                   │
│  Proposal                              none yet │
│    Nothing is applied until you accept it.      │
│  Activity                                       │
│    fixture/provider · full-access · medium      │
│  ○  Ready     describe the desired filter       │
│  [ Submit ]                                     │
└─────────────────────────────────────────────────┘
```

### 12.18 Investigation 🧠 `I` — class L

Before: `[ Start ]` alone above the fields; boxes `Question or follow-up`,
`State`, `Activity and saved investigations`.

After, 100x30 (86 × 18):

```
┌ Investigation 🧠 ──────────────────────────────────────────────────────────────────┐
│                                                                                    │
│   New │ Saved                                                                      │
│                                                                                    │
│  Question     Why did request 19 take 57ms?▁                                       │
│               ▁                                                                    │
│               ▁                                                                    │
│                                                                                    │
│  Transcript                                                            0 messages  │
│    Start an investigation on a fixed snapshot of this view; follow-ups reuse it.   │
│                                                                                    │
│  ○  Ready     enter a question for a new fixed snapshot                            │
│                                                                                    │
│  [ Start ]                                                                         │
│                                                                                    │
└────────────────────────────────────────────────────────────────────────────────────┘
```

Once a session exists the primary button is `[ Send ]`, `[ New snapshot ]`
follows, the Transcript pane fills the body (scrollable, `n messages`), and the
`Saved` segment lists saved investigations as a selectable pane with `[ Open ]`.

### 12.19 Folding `z` — class M

New with the fold-by-column work, so there is no "before": folding used to be a
palette toggle with a built-in policy and no surface of its own.

The whole model on one screen: **a run is a group of rows sharing one key, and
the key is the value of exactly one column.** The default column is a derived
one, `Message pattern` — the row text with volatile substrings replaced and the
level prefixed — which is what folding always keyed on, so a view that never
opens this dialog folds exactly as it did before.

After, 100x30 (72 × 15):

```
┌ Folding · Raw events ────────────────────────────────────────────────┐
│                                                                      │
│  Fold repeated  [x] on                                               │
│  Key column     Message pattern                             ▾        │
│  Minimum run    3 or more                                   ▾        │
│  Scope          Adjacent runs only                          ▾        │
│  Normalisation  Standard                                    ▾        │
│                                                                      │
│  ●  Applied   folding on Message pattern · 1 run collapsed · 39 rows │
│               hidden                                                 │
│  The pattern column is the row text with timestamps, ids and numbers │
│  replaced. Any other column folds on its value unchanged.            │
│                                                                      │
│  [ Collapse expanded runs ]                                          │
│                                                                      │
└──────────────────────────────────────────────────────────────────────┘
```

`Normalisation` is drawn **only** while the key is `Message pattern`. It is the
one setting that cannot affect any other key: a real column supplies its value
as it stands, so nothing about it is rewritten and no field the user did not ask
about is replaced. Offering a control that would silently do nothing is worse
than not offering it (§12.4 makes the same argument about data-relative windows).

Open key-column picker (class A, anchored under the field):

```
│  Key column     Message pattern                             ▾        │
│                 ┌───────────────────────────────────────────┐        │
│  Minimum run    │ Message pattern   (default)               │        │
│  Scope          │ host                                      │        │
│  Normalisation  │ service                                   │        │
│                 │ [ New column… ]                           │        │
│                 └───────────────────────────────────────────┘        │
```

`[ New column… ]` is how a user folds on *several* fields, and it is the only
way, on purpose. The picker becomes a checkbox list of the view's columns;
space picks them in order, and Enter opens the ordinary enrichment step editor
on a concatenation of them:

```
┌ Enrichment › New step ───────────────────────────────────────────────┐
│  Expression   fold_key = pl.concat_str([pl.col('host').cast(pl.Strin │
│               g), pl.col('service').cast(pl.String)], separator="|",  │
│               ignore_nulls=True)▁                                     │
```

The user reads it, edits the separator or the null handling if their data needs
it, and saves it as an ordinary step. When the view accepts the step, Folding
selects the column it created. There is no second field-combination mechanism to
keep in sync with enrichment, and nothing here can produce a column the
enrichment list does not also show.

Every control takes effect where it stands; there is no `Apply`. Folding is
reversible presentation over rows that are never touched, the policy is read
from the view on every frame, and Escape is not an undo. The action row
therefore holds only `Collapse expanded runs`, which is the one verb that is not
a setting.

Default action (§8.9): **`Collapse expanded runs`**, filled. A settings dialog
still declares a default — §8.9 reserves "no default" for a dialog with no
action row (Help), and a drawn button that the dialog refuses to call its
default would leave the row unmarked for no reason. What the declaration buys
here is smaller than usual and is worth stating rather than hiding: every
control in this body is a checkbox or a closed dropdown, and §8.9's own table
gives Enter to each of those, so nothing hands Enter on and the fill is doing
marking work rather than routing work. The default is still named in one
function that `render` (which button to fill) and the Enter arm (which verb to
run) both read, so a non-consuming control added later cannot disagree with the
fill. Space presses nothing (§8.2).

Live region (§5.2.1): the **key-column picker**. Its rows come from a bounded
sample of the view's rows read through `unfolded_page`, so a source that is
still arriving can add a column while the list is open, and compose mode gains
and loses checkmarks under the cursor. It reserves its rows from the frame and
takes its width from the field it drops from, so the popup rect is identical
frame to frame; an overlong list scrolls behind a trailing `+N more` — the
§5.2.1 affordance for a region with no §8.7 heading to carry a count — and a
short one leaves the remaining rows blank. The other three dropdowns are over
fixed option sets and are not live.

After, 54x16 (52 × 12; help and pads dropped, `Normalisation` absent because the
key is a column):

```
┌ Folding · Raw events ────────────────────────────┐
│  Fold repeated  [x] on                           │
│  Key column     service                     ▾    │
│  Minimum run    3 or more                   ▾    │
│  Scope          Adjacent runs only          ▾    │
│  ●  Applied   folding on service · 2 runs coll…  │
│  [ Collapse expanded runs ]                      │
└──────────────────────────────────────────────────┘
```

### 12.20 Details `d` — docked pane, not a dialog

Apply only: label/value columns (`id`, `raw`, then fields, label_w 10), a
scrollbar in the pane's last column when it overflows, and no `↑/↓ scroll`
footer. Its title stays `Details`.

§8.11: for a JSON record the field rows are the tree, with the same
disclosure glyphs, cursor and per-view expansion memory as Fields; while the
pane has focus Up/Down move the cursor (they scroll the pane when the record
is not a tree), Enter/Right/Left open, close and climb, and the palette's
`Expand or collapse value` row is the same operation.

**Colour comes from the record, not from the pane.** Details and the log show
the same records, so nothing about colour is decided twice:

- `details::json_kind_style` is the one function that turns a JSON token into a
  colour, and the log line calls it too. A number cannot look like one thing in
  the line and another in the tree.
- `ui::styled_record_text` is the log's own text styler, unclipped. The `raw:`
  line and every flat value go through it, so the pane's copy of a record's text
  and the log's copy are the same text in the same colours.
- `ui::record_style` gives the pane the record's row colour — the view's colour
  field hashed, else severity — which everything that is not a JSON token
  inherits. The tree cursor overrides it exactly as the log's selection does.
- A column's name takes the identity colour its key carries inside the raw line,
  so `level:` in the pane and `"level"` in the log are one colour: they name the
  same column. The pane's own caption, `stable display id`, is not a column and
  stays chrome. `command.status` keeps its pending/applied colours, because that
  is command run state rather than a colour of the record.

Two consequences that are easy to get wrong:

- The pane draws on `base_bg`, not `dialog_bg`. It is docked in the workspace
  rather than floating over it, and `Theme::value_color` lifts an identity
  colour until it clears `MIN_IDENTITY_CONTRAST` **against `base_bg`** — on any
  other surface that measurement is of something the user is not looking at.
- `record_style` is called with `selected = false`. The record the pane shows is
  by definition the selected one, and repeating the log's selection highlight
  would tell the user where the cursor is, which the log already does.

Anything new that decides what colour a record is belongs in those functions, so
every surface showing the record picks it up together.

---

## 13. Implementation notes

Shared primitives (new module, e.g. `crates/lvu/src/dialog_layout.rs`;
`dialog_controls.rs` keeps buttons and styles):

```
enum DialogClass { S, M, L, XL, P }
struct DialogSpec<'a> { title: Cow<str>, class: DialogClass, header: Option<Header>,
                        body: Body, message: Option<Message>, help: Option<&str>,
                        actions: &[ActionSpec] }
struct DialogRegions { popup, interior, content, header, body, message, help, actions: Rect,
                       body_scroll: Option<ScrollState> }
fn dialog_rect(area: Rect, class: DialogClass, rows: u16) -> Rect        // §5.2
fn live_rows(area: Rect, class: DialogClass, stable: &DialogContent, desired: u16) -> u16  // §5.2.1
fn regions(area: Rect, spec: &DialogSpec) -> DialogRegions               // §3 arithmetic
fn form_rows(content: Rect, rows: &[FormRow]) -> Vec<FieldRects>         // §4.2 incl. reflow
fn pane(area: Rect, heading, count: Option<&str>, lines: usize, scroll) -> (Rect heading, Rect viewport, Option<Rect> scrollbar)
fn scrim(buffer: &mut Buffer, area: Rect, theme: Theme)                   // §6.2
```

`hit_regions` are populated from the same `DialogRegions`/`FieldRects`; no
dialog computes a rect of its own. `selection_modal = interior`.

Acceptance (TestBackend + PTY):

- For each dialog × {140x40, 100x30, 80x24, 54x16} × {love-dark, love-light,
  terminal}: popup within `area`; `popup.width` equals the §5.3 table; height
  ≤ class max and equals `rows + 2` when it fits; no two widgets write a
  non-space glyph to the same cell (diff the buffer after each widget);
  `input_bg` cell count equals the sum of field rect areas; scrollbar present
  iff content > viewport; message row present iff the dialog has state; no
  string from the banned footer vocabulary (`Enter`, `Tab`, `Esc`, `PgUp`,
  `PgDn`, `Home/End`, `↑/↓ scroll`) anywhere.
- Overflowing single-line input renders the tail once with a leading `…` and
  the caret inside the field (regression for `dialog-design.md` §3).
- §5.2.1: driving a path character by character through Add source at 80x24 and
  54x16 leaves the popup rect bit-identical on every keystroke, including the
  frames where the debounced scan is pending and where it returns nothing.
- Settings at 80x24 and 54x16 exposes every field and every effective-values
  row by scrolling; `dialog_scroll_limit > 0` and a wheel hitbox exist.
- View at 54x16 with a long name: buttons and text never share a cell.
- Time at 54x16: no `Scroll up`/`Scroll down`; Start/End reflow to six rows;
  every field's caret is visible when focused.
- Scrim: with a dialog open, no cell outside `popup` has a foreground other
  than `muted`/`border` (dark and light).
- Unicode: labels containing 🧠, `界`, `é` keep field and button columns aligned
  (`wcswidth` on the PTY screen).
- Default action (§8.9), for every dialog with an action row, at 80x24 and
  54x16: exactly one button's cells carry `bg = accent`, and it is the button
  the dialog's default function names; Enter from the dialog's initial focus
  either runs that action or lands on one of the §8.9 exceptions; every list
  has a `›` row on the first frame whenever it has rows.

---

## 14. Not resolved here

- Exact wording of every state sentence for Ask/Investigation/Source-agent
  stages beyond those shown; follow §7.4 and keep them under one wrapped row at
  60 columns.
- The love themes' `dialog_bg` could also be nudged (`#2c2029` dark, `#f6e4e0`
  light give 1.14–1.17 against base) if the scrim alone is judged insufficient
  once seen in a real terminal; measure before changing.
- Whether Recipes' `Update` and `History` warrant staying in the first row or
  belong under `More ▾`; decide on real usage.
- The Enrichment two-layer work in flight should adopt §12.5/§12.6 directly;
  if its step editor needs more than the add-step row, it becomes the child
  layer and External command a second segment of that child, never a grandchild.
