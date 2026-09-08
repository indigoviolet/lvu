# Discoverability — audit against `dialog-system.md` §8.10

Status: the rule is on main (preview 052) and applied everywhere. The
dialogs marked legacy below have since been converted with the mnemonics and
palette rows listed; only Raw context is still held. Companion to
`dialog-default-actions.md` (§8.9).

**Superseded in one respect.** This document says Alt+letter is what presses a
button. It is now the *fallback*: the bare underlined letter presses it
whenever no text field has focus, and Alt+letter is for when one does. The
reason is measured rather than stylistic — an xterm with its default
`metaSendsEscape: false` sends Alt-f as the 8-bit character U+00E6, not as a
chord, so Alt was never reliably deliverable. `dialog-system.md` §8.10 holds
the current rule and the terminal-encoding table behind it; the tables below
still describe which letter each button marks, which is unchanged.

The rule in one paragraph: a user learns what they can do in four places, each
with one job. The **action row** shows the operations of this dialog, with the
Alt-letter that presses a button underlined in its label and never printed as
a chord. The **hint line** says what the content means and what a control
cannot say for itself, and may name a key only for a non-routine operation
that has no button and belongs to this dialog alone. The **palette** is the
exhaustive index: every operation, with the chord that works in the current
focus. **Help** indexes the base screen — the keys with no visible control —
and states the routine conventions once. Nothing prints Enter, Esc, Tab,
arrows or Space anywhere else, because §8.9 and the shell's dismissal rule
make them uniform.

"Before" is `main` at `b643433`. Row order follows the anatomy: base screen,
then dialogs.

## The base screen

| Surface | Shows today | Shows now |
| --- | --- | --- |
| Sidebar `Sources / views` | heading, view rows, no hints | unchanged |
| Log pane heading | `Log viewport` (`· x=N` while panned), column headers | unchanged |
| Status line | `FOLLOW \| … \| 6-24/24 \| ? help` | `… \| ? help · Ctrl-P commands` — the two doors, and the only chords on the base screen |
| Details pane | `↑/↓ scroll` footer on its last row | no footer; the row is content |
| `NO VIEW` line | `add or discover a source to begin \| ? help` | `… \| ? help · Ctrl-P commands` |
| `terminal too small` | `q quit` | unchanged: one operation, one hint |
| Folding notice | `runs of N or more collapse; Enter expands one` | `…; select a run to expand it` |
| Sidebar focus | only `j`/`k`/Tab/`q`/`?` bound; every other key dead, so the palette printed blank shortcuts | the sidebar binds only its own keys (`j`/`k`, and Left/Right/Enter are inert); every other base key means what it means in the log, so a chord the palette prints works from either pane |
| Details focus, palette `Quit` row | printed `q`, but `q` hides the pane | prints `Ctrl-C`; the key table now says what the shell does |

## The command palette

| Before | Now |
| --- | --- |
| `Add source` had no shortcut (`n` mapped to a forwarding action the catalog did not name) | `n` |
| `Settings` had no shortcut (`,` missing from the candidate table) | `,` |
| `Expand or collapse group` had no shortcut | `Enter` |
| First / last record (`g` / `G`) had no palette entry | `Jump to first record` `g`, `Jump to last record` `G` |
| Previous / next quiet period (`{` / `}`) had no palette entry | `Previous quiet period` `{`, `Next quiet period` `}` |
| Layer chords printed in mixed case (`Alt-a`, `Alt-s`) | `Alt-A`, `Alt-S`: one spelling |
| External command rows printed `Alt-s`/`Alt-r`/`Alt-d`, none of which was bound (the keys were Ctrl-S, Ctrl-R, Alt-Delete) | `Alt-S`, `Alt-R`, `Alt-M`, all bound and all underlined on their buttons; the Ctrl chords and Alt-Delete keep working unlisted |
| View rows printed `Alt-D` (Clone) and `Alt-M` (Sources), letters not in the labels | `Alt-C`, `Alt-S`; `Alt-D`/`Alt-M` keep working unlisted |
| Fields rows printed `c`, `r` | `Alt-C`, `Alt-R` (the mnemonics); `c`/`r` keep working; `Pin` stays `Space` |
| Storage cleanup printed `c` | `Alt-C`; `c` keeps working |

Not changed, recorded: `Scroll details up/down` rows have no shortcut because
their payload (six rows) differs from the arrow keys' (one row), and arrows
are a convention anyway; `Complete editor field or value` prints `Tab`
(Advanced) and `Ctrl-Space` (step editor), the two non-routine chords the hint
line may also name; the demo fixture's `a` is not an operation.

## Help

| Before | Now |
| --- | --- |
| Six sections mixing base keys with dialog internals: `Alt-C in Enrichment`, `i … Space pins, c colors, r correlates`, `Ctrl-P Fold`, `Alt-F / Alt-C`, `Ctrl-D`, `Ctrl-A`, `Alt-M`, `Alt-B`, `Alt-D`, `Alt-R` (rename), `Alt-N` | Five sections: EVERYWHERE (the doors, settings, redraw, quit); **CONVENTIONS** (Enter, Esc, Tab, Space, Alt + underlined letter — stated once, nowhere else); LOG (the keys with no control: `g`/`G`, `{`/`}`, pan, Enter on a group, `[`/`]`, Tab, `f`, `d`, `b`); OPEN (the key that opens each dialog); SOURCES (`Alt-S`/`Alt-R`, the two base-screen chords without a control) |

## Every dialog

Buttons are listed as drawn; an underlined letter is written `_x_`. "Hint" is
the §3 help region.

| # | Dialog | Buttons today → now | Hint today → now | Chords now shown only by button / palette |
| --- | --- | --- | --- | --- |
| 1 | Search | none | `Examples: text · "field name": text · /regex/ims · \/literal` (unchanged) | — |
| 2 | Advanced filter | none | `Use a Polars expression. Fields and sampled literals complete with Tab.` (unchanged: the one non-routine key with no button; the palette prints `Tab` too) | — |
| 3 | Grouping | `Apply` | unchanged | — |
| 4 | Time window | `Apply` `Clear` `🧠 Recognize timestamp` → `Apply` `_C_lear` `🧠 Recognize _t_imestamp` | unchanged | Alt-C, Alt-T on the buttons; Alt-A/P/E/U/5/M/H (basis and window presets) in the palette, where they were |
| 5 | Enrichment | `Add` `Edit` `Remove` `External command…` → `_A_dd` `_E_dit` `_R_emove` `External _c_ommand…` | unchanged | Alt-A/E/R/C were printed in Help as `Alt-C in Enrichment`; now on the buttons and in the palette |
| 6 | Step editor | `Save` (`Remove`) | `name = expression or /regex…/` (unchanged) | Ctrl-Space completion: palette only (no button, one dialog) |
| 7 | External command | `Save` `Review and run` `Remove` `New line` → `_S_ave` `_R_eview and run` `Re_m_ove` `_N_ew line` | unchanged | the palette's three rows printed chords that did not exist; fixed (above) |
| 8 | Add source | `Open`/`Request proposal`/`Start reviewed source` (+ `Rescan`) | none | Alt-F/Alt-C are the File/Command radios (not buttons; palette rows), Ctrl-D/Ctrl-R palette rows; Help no longer repeats them |
| 9 | View | `New blank` `Clone` `Rename` `Sources` `Apply` → `New _b_lank` `_C_lone` `_R_ename` `_S_ources` `Apply`; the active mode is bold, no longer underlined (the underline is the mnemonic's) | unchanged | Alt-B/C/R/S; Help no longer lists Alt-M/B/D/R |
| 10 | Recipes | `Apply` `Save` `Update` `History` `More ▾` → `Apply` `_S_ave` `_U_pdate` `_H_istory` `More ▾`; suggestion row `_A_dapt` `Reject` | History mode: `Enter applies the selected revision; …` → `Applying a revision leaves the recipe's own pointer unchanged.` | Alt-S/U/H/A on buttons; Alt-B/I/E/G and `x` in the palette |
| 11 | Fields | `Pin` `Color rows by field` `Correlate across sources` / `Raw context` → `_P_in` `_F_ilter` `E_x_clude` `_C_olor` `Fol_d_` `Co_r_relate` / `Raw c_o_ntext` (§8.12 grew the row; the short verbs read against the Value pane) | `Escape cancels the lookup; the view you are in does not change.` → `The view you are in does not change while the lookup runs.` | Alt-P (new), Alt-C, Alt-R, Alt-O; `Space`, `c`, `r`, `o` keep working; Help no longer lists `Space pins, c colors, r correlates` |
| 12 | Storage | `Refresh` `Preview cleanup` → `_R_efresh` `Preview _c_leanup` / `Confirm _c_leanup` | none | Alt-R, Alt-C; `r`, `c` keep working |
| 13 | Settings | `Save` (`More`) | none | — |
| 14 | Help | none | — | rewritten (above) |
| 14a | Folding (landed on main during this work) | `Collapse expanded runs` | column-composer message `space picks a field · Enter builds the column` → `choosing the fields that make up the new column` (the keys are the conventions) | opened from the palette only (`Folding`, `Fold repeated events`); Help's OPEN section does not list it because it has no key |
| 15 | Palette | none | the selected row's description | — |
| 16 | Raw context (legacy) | `Back to anchor` | none | `g` stays the unlisted accelerator; the conversion adds `Alt-B` as the mnemonic (`_B_ack to anchor`) and the palette row |
| 17 | Bookmarks (legacy) | `Go to` `Edit note` `Raw context` `Remove`; Note child `Save note` | list: unchanged; Note child: `Notes are capped at 1024 bytes and saved with the view. Escape leaves the note unchanged.` → first sentence only | conversion: `Edit _n_ote` (Alt-N; today Alt-E), `Re_m_ove` (Alt-M; today Alt-D), palette rows for both; the Tab ring must include `Go to` |
| 18 | Ask 🧠 (legacy) | `Submit`/`Apply`/`Cancel request` | task help (unchanged) | conversion: Alt-F/Alt-E select the Kind (dropdown, palette rows); Alt-T becomes a palette row only; Ctrl-Enter submits from the Request field (§8.9) |
| 19 | Investigation 🧠 (legacy) | `Start`/`Resume`/`Send` (`Open`, `New snapshot`) | unchanged | conversion: `New _s_napshot` (Alt-S; today Alt-N, which Ask uses for newline — §8.1 says Alt-N is the newline everywhere) |
| — | Correlation (legacy) | `Correlate` `Cancel` | none | conversion: palette rows for Correlate and for the field dropdown |
| — | Completion popup (Advanced, step editor) | none | `↑/↓ select` footer → none | — |
| — | Add source, 🧠 preview pane | heading `Preview · lines a–b of n · ↑/↓` → `Preview · lines a–b of n` | | — |

## What none of them repeats

- A chord is printed in at most one of {button, hint line, Help}; the
  palette may print any chord because it is the index.
- The routine keys are printed once, in Help's CONVENTIONS, and the
  `dialog_controls::action_line` key-list primitive is deleted so no footer
  can come back through it.
- Descriptions live in the palette; hint lines say what content means.

## PTY assertions changed, never weakened

- `test_shared_inspection_dialogs_pty`: asserted the Details footer `↑/↓
  scroll` is present → asserts it is absent; the Help bottom anchor is
  `Alt-R` (the last entry) instead of `Alt-N` (retired).
- `test_help_pty`: bottom anchor `Alt-N` → `Alt-R`; adds `CONVENTIONS` present
  and six retired dialog-internal chords absent.
- New `tests/pty/test_discoverability_pty.py`: the mnemonic underline on a
  real terminal (`[ Add ]` underlines only `A`), the two doors on the status
  line, no Details footer, the palette printing `n` beside `Add source` and
  `,` beside `Settings`, Alt-C cloning a view.
- Rust: `crates/lvu/tests/discoverability.rs` walks every base operation and
  asserts a palette row with that chord in both the log and sidebar focus;
  Help content; no dialog hint names a routine key; the mnemonic primitive.
