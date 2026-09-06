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
| Actions | Colored shortcut footer, with verified bindings and readable action names |

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
is centered beneath the full sidebar width.
