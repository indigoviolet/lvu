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
| Discovery | Base foreground for path/scan diagnostics; source correction validated, publication pending |
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

## Time form — behavior published in preview037, layout correction pending

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

The published renderer still stacks segments vertically and displays inactive
More controls. Correct it to Start [date] [time] [timezone] and End equivalents,
with a separate Apply/Clear/Recognize action row. Timezone becomes a staged
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
