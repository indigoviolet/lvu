# Dialog default actions — audit against `dialog-system.md` §8.9

Status: the rule is on main (preview 051) and applied to every dialog.
Rows 17 to 19 were audited as legacy dialogs; Bookmarks, Ask 🧠 and
Investigation 🧠 have since been converted with the obligations listed, and
only Raw context (row 16) is still held, pending `raw-context-as-jump.md`.
Companion to `dialog-discoverability.md` (§8.10).

The rule, in one paragraph: every dialog with an action row declares **one
default action**; it is the one button drawn with the accent fill (§6.3);
Enter executes it from anywhere except inside a multi-line text field
(newline; Ctrl-Enter is the accelerator), an open popup (commits its row), a
closed dropdown (opens), a checkbox/radio/segment (toggles), another button
(presses itself) or a pending confirmation (confirms); a destructive action
is never the default; the default may change with state and the fill moves
with it; every list opens on a real row.

"Before" is the code on `main` at `965eee3`. "Proposed" is what this branch
ships for component dialogs and what the conversion must ship for legacy ones.

## The eighteen dialogs

| # | Dialog | Owner | Buttons (drawn order) | Default before | Enter before, from the initial focus | Enter before, elsewhere | Initial selection before | Proposed default (§8.9) | Change made / required |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | Filter `/` › Search tab | component | `Apply` `Clear` (since the two-tab Filter dialog, §12.1; before it: none) | implicit Apply | field: applies | completion open: accepts the row; tab control: Enter selects the focused segment (§8.9) | n/a | **Apply** (filled) | action row added with the tabs; the earlier "no button" exception is gone. Alt-Enter inserts a newline (the search draft is single-line, so it is inert there). |
| 2 | Filter `/` › Advanced tab | component | `Apply` `Clear` (§12.1) | implicit Apply | field: applies | completion open: accepts; Alt-Enter newline (multi-line draft) | n/a | **Apply** (filled) | as row 1; `p` is retired |
| 3 | Multiline grouping `m` | component | `Apply` | Apply (index 0, accent text) | field: applies | Alt-Enter newline | n/a | **Apply** (filled) | fill only |
| 4 | Time window `t` | component | `Apply` `Clear` `🧠 Recognize timestamp` | Apply (index 0) | Basis dropdown: **opens the list** (§8.9 exception, unchanged) | date/clock/zone segments: **nothing** (`TimeAction::None`); Apply/Clear/Recognize: themselves; open dropdown: commits | dropdown highlight = current value (verified) | **Apply** (filled) | Enter in the six text segments now applies. `TimeAction::None` deleted. |
| 5 | Enrichment `e` | component | `Add` `Edit` `Remove` `External command…` | Add (index 0, accent text) | Steps list: `Edit` selected, or a **notice** "no enrichment step is selected; use Add" on an empty chain | buttons: themselves; Alt-A/E/R/C accelerators | view-owned `enrichment_selected`, **not clamped** on open (a shrunken chain could index past the end) | **Edit** when a step is selected, **Add** when the chain is empty; the fill moves | `EnrichmentDialog::default_control` names it; Enter on the list runs it; selection clamped on open. Worked example. |
| 6 | Enrichment › step editor | component (child) | `Save` (+ `Remove` when editing) | Save (index 0) | Expression field: saves; completion open: accepts | Input/Output panes: **nothing**; Remove: removes; Alt-N newline (multi-line) | preview record = selected row (verified) | **Save** (filled) | panes hand Enter to Save. Enter in the Expression field stays "save": it is a multi-line *expression* field (§8.9 second row), Alt-N is its newline, and every enrichment suite submits it with Enter. |
| 7 | External command | component | `Save` `Review and run` `Remove` `New line` | Save (index 0) | Program field: **confirm_run** — a no-op unless a review is pending | Arguments/Environment (multi-line): confirm_run, i.e. **nothing**; Shift-Enter/Alt-N newline; Ctrl-Enter save; Alt-Enter review; buttons: themselves | n/a (results pane is not a list) | **Save**; **Review and run** while a review is pending (fill moves; Enter confirms from anywhere, including the multi-line fields, because the review is a pending confirmation) | Enter in Program/Directory saves; Enter in Arguments/Environment inserts a newline (was inert); `Review and run` confirms a pending review instead of re-preparing it (Ctrl-R still re-prepares). |
| 8 | Add source `n` | component | `Open` / `Request proposal` / `Start reviewed source` (+ `Rescan` in Discover) | index 0 | Path/Filter/Describe field: submits; a highlighted suggestion opens it | segments/radios/Rescan: themselves (§8.9 exception) | suggestions 0, candidates 0 (verified) | index 0 (filled) | fill only |
| 9 | View `v` | component | `Apply`/`Apply membership`; `New blank │ Clone │ Rename │ Sources` is the header control | **`New blank`** was styled primary (index 0) although Apply is the verb | Name field: applies | Sources list: applies; header segments switch mode without submitting | membership list row 0 (verified) | **Apply** (filled, the sole action) | Modes moved into the shared segmented header. One header focus stop replaces four faux buttons; `render_action_row` draws only Apply. Also fixed: at 80x24 the old gutter mismatch dropped `[ Apply ]` off the screen entirely. |
| 10 | Recipes `r` (+ History) | component | `Apply`/`Save revision`/`Review import`/`Export revision`/`Apply revision` first, then mode buttons, `More ▾` | index 0 | list: applies | field: applies; buttons: themselves; open `More ▾` menu: commits | list 0 on load; returning from History keeps the selection (verified) | index 0 (filled) | fill only |
| 11 | Fields `i` | component | `Pin`/`Unpin` `Filter` `Exclude` `Color` `Fold` `Correlate` (or `Inspect context` when the record has no fields) | index 0 | list: pins (Space too); a container row (§8.11) consumes Enter to open or close | buttons: themselves | list 0 (verified) | **Pin** (filled) | fill only; the row grew with §8.12 |
| 12 | Storage `S` | component | `Refresh` `Preview cleanup`→`Confirm cleanup` | Refresh (index 0) | list: **nothing** (Enter fell through to the shell) | `r` refresh, `c` twice cleanup | list 0 (verified) | **Refresh** (filled, stays the default while `Confirm cleanup` shows: destructive is never default) | Enter refreshes |
| 13 | Settings `,` | component | `Save` (+ `More` when the body overflows) | Save (index 0) | Provider text field: **nothing** | number fields: nothing; Theme: opens dropdown; toggles: toggle (Space too); More pane: nothing; Save: saves | theme dropdown highlight = current theme (verified) | **Save** (filled) | Enter saves from every text field. `More` is a button whose press is the pane focus it already holds, so Enter on it stays inert (`test_settings_pty` guards that it never saves). Space no longer routes through `activate`, so it can never save; it toggles/opens exactly as before. |
| 14 | Help `?` | component | none | none | inert | inert | n/a | **none** — no action row, Enter inert | none |
| 15 | Command palette Ctrl-P | shell | none (class P list) | run selected | runs the selected enabled command | disabled row: nothing, description says why | row 0, or the previously selected command after a context refresh (verified) | **run selected**; the selection highlight is its marking (there is no button row) | none |
| 16 | Raw context `o` | **legacy** | `Back to anchor` | index 0 (accent text) | list: **nothing** (no Enter arm; `g` is the only route; the drawn hitbox is never read) | — | anchor row, page offset −5 (verified) | **Back to anchor** (filled) | conversion must: add the Enter arm; make the recorded hitbox answer `hit()`. |
| 17 | Bookmarks `B` (+ Note child) | component | `Open in All events` `Edit note` `Inspect context` `Remove` (destructive); child: `Save note` | Open in All events (index 0) | list: **jumps** permanently; child Input: saves (verified) | buttons: themselves; `Inspect context` makes the temporary jump that returns with `o` | list 0 (the first bookmark, not the current record) | **Open in All events**; child **Save note** | Enter stays the permanent jump; the explicit labels distinguish it from temporary inspection. |
| 18 | Ask 🧠 `A` | component (was legacy at audit) | one of `Submit`/`Submit again` · `Apply` · `Cancel request` (by stage) | index 0 | Request (multi-line): **newline**; **no submit accelerator** exists | Kind dropdown closed: opens; open: commits; scroll pane: nothing; button: itself | kind dropdown highlight = current kind (verified) | **Submit**, then **Apply** with a proposal, then **Cancel request** in flight (fill follows) | conversion must: add Ctrl-Enter = default inside Request; make the scroll pane hand Enter to the default. |
| 19 | Investigation 🧠 `I` | component (was legacy at audit) | `Start`/`Resume`/`Send` (+ `Open` in Saved, `New snapshot`) | index 0 | Question (multi-line): **newline**; **no submit accelerator** | New/Saved segments: select; transcript pane: nothing; buttons: themselves; Alt-N is *New investigation* here but *newline* in Ask | saved list 0 (verified) | **Start/Resume/Send** (fill follows) | conversion must: add Ctrl-Enter = default inside Question; pane hands Enter to the default; reconcile Alt-N with Ask (§8.1 says Alt-N is the newline). |
| 20 | Colour rules `c` | component | `Add` `Remove` `Apply` | — (landed after this audit) | Predicate field: applies | list: edits the selected rule; colour chooser: cycles it; buttons: themselves; Alt-A add, Alt-R remove | rule list row 0, or the Add button on an empty list ✓ | **Apply** (filled); `Remove` drawn destructive so it can never be the default | built to the rule: `render_actions` with the Apply index and `Remove` in `destructive` |

| 20 | Folding `z` | component | `Collapse expanded runs` | — (dialog did not exist) | — | — | key-column list opens on the current value (verified) | **Collapse expanded runs** (filled) | new dialog, built to the rule. Every field takes effect where it stands, so there is no `Apply` and the row holds one verb; that verb is the default. Every control in the body is a checkbox or a closed dropdown, so §8.9's table gives Enter to each of them and nothing hands Enter on — the fill marks the row rather than routing Enter. Space presses nothing. |
| 21 | View summary `V` | component | `Open` | — (dialog did not exist) | — | — | list row 0, the view itself (verified) | **Open** (filled) | new dialog, built to the rule. Read-only: the one verb navigates to the dialog that owns the selected row and *replaces* the summary with it. Enter from the list and from the button both run it; `o` (bare or with Alt) is the mnemonic. |

(The task counts eighteen dialogs; Details `d` is a docked pane, not a dialog,
and Correlation is a dialog the list did not name — its `Correlate`/`Cancel`
row already filled index 0 and its list Enter opens the field dropdown, which
§8.9 permits. Its conversion (W21) kept both and named the default in
`default_control()`, as the rule asks. Colour rules did not exist when the audit
was written and is listed because it ships under the rule rather than carrying
an obligation into a conversion.)

A settings-only dialog is not an exception to §8.9. "No default" is for a
dialog with **no action row** — Help. A dialog that draws a button and then
declines to call it the default leaves its row unmarked and its Enter arm
unowned for no benefit. Folding is the first of this shape; if a second arrives
whose action row is a genuine no-op from every control, the question to reopen
is whether it should have an action row at all, not whether it should have a
default.

## Behaviour the user relies on, kept

- **Enter in the editors applies.** Search, Advanced and Grouping submit on
  Enter with the completion popup closed; the popup takes Enter while open.
- **Enter on a focused button presses that button**, never the default. The
  Enrichment PTY suites reach `External command…` by four Tabs and Enter.
- **Enter in a focused dropdown opens it** (Time basis/window/zone, Settings
  theme, Ask kind) and commits while open. Every Time suite drives the dialog
  this way.
- **Enter on a toggle toggles it** (Settings' three checkboxes, Source's
  File/Command radio, the mode segments). Space does the same and is now the
  only thing Space does there.
- **Enter in an expression field applies it** — Filter › Advanced, Grouping
  and the step editor's Expression are multi-line for wrapping, not for
  prose; Alt-Enter / Alt-N stay their newline. §8.9 names them as the second
  kind of multi-line field so the rule is precise rather than an exception.
- **A pending run review is confirmed by Enter from anywhere** in External
  command, including from the multi-line fields — `test_command_enrichment_pty`
  sits in Arguments when it confirms. Escape drops the review.
- **Enter with a highlighted path suggestion opens it** (Add source).

## Behaviour changed on purpose

| Where | Before | After | Why |
| --- | --- | --- | --- |
| Enrichment, empty chain, Enter on the list | notice "no enrichment step is selected; use Add to create one" | opens the new-step editor | the default is `Add` when there is nothing to edit |
| Time, Enter in a date/clock/zone segment | nothing | applies | a text segment does not consume Enter |
| Settings, Enter in Provider/Mode/Thinking/cache-limit fields | nothing | saves | same (Enter on the `More` button stays inert: it is a button, and its press is the focus it holds) |
| Settings, Space on a text field | nothing (routed through `activate`, which ignored it) | nothing (no longer routed through `activate`) | Space must never execute the default |
| Storage, Enter on the list | fell through to the shell | refreshes | the list has no row action; Refresh is the default |
| Step editor, Enter on the Input/Output pane | nothing | saves | a pane does not consume Enter |
| External command, Enter in Program/Directory | run confirmation (no-op without a review) | saves | Save is the default |
| External command, Enter in Arguments/Environment, no review pending | run confirmation (no-op) | newline | §8.1; Ctrl-Enter saves, Alt-Enter reviews, Shift-Enter/Alt-N also newline |
| External command, `Review and run` with a review already pending | re-prepared a fresh review | confirms the pending one | the button's label is the default's verb; Ctrl-R still re-prepares |
| View, primary marking | `New blank` (first button) carried the accent | `Apply` carries the fill | the verb, not the first mode, is the default |
| Every action row at 80 columns | `packed_button_rows` measured a 1-column gutter, `render_action_row` drew a 2-column one; View's `[ Apply ]` wrapped onto a row that did not exist and vanished at 80x24 | one `BUTTON_GUTTER` for measuring and drawing | a default that is not on screen cannot be filled |

## Primary treatment (the one decision that changes the look everywhere)

Chosen: **accent fill** — `bg = accent`, bold, foreground pushed to ≥ 4.5:1
(`dialog_bg` for the RGB themes, `selection_fg` for the terminal theme). A
focused default takes the one focus style every control has; the fill
returns when focus leaves. No glyph and no bracket change, so every
`"[ Apply ]"` assertion, click target and focus-byte prefix in the suites is
untouched, and the marking degrades to bold under `NO_COLOR`.

Considered and not chosen: keeping the accent *text* (what `main` had: the
first button in `accent` bold) — it is the same colour as the focused-row
`accent` labels and does not read as a button at all in the love themes;
bracket glyphs (`< Apply >`, `[[ Apply ]]`) — survive `NO_COLOR` but change
widths and every button-text assertion, and are not what a GUI does.

## Where the rule lives in code

- `dialog_controls::ButtonRole`, `role_style`, `render_role_button`,
  `ActionRow`, `BUTTON_GUTTER` — the look and the row spec.
- `ui::render_actions` (explicit default index) and `ui::render_action_row`
  (default = first) — every action row in the product, legacy included.
- `ui::render_enrichment_button` — the `button_layout` rows the two
  enrichment layers draw; takes a `default` flag from the same function the
  Enter arm reads.
- Per component: `EnrichmentDialog::default_control`,
  `ExternalCommandDialog::review_pending`, `SettingsDialog::focus_consumes_enter`,
  Time's `open_focused` table, `StorageDialog::key` (Enter), View's Apply index.
- Tests: `crates/lvu/tests/default_actions.rs` (role, every changed dialog,
  one fill per dialog at 80x24 and 54x16) and `tests/pty/test_default_actions_pty.py`
  (Enter from the initial focus, initial selection, the fill on a real
  terminal at 80x24 and 54x16).
