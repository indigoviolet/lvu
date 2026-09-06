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
is centered beneath the full sidebar width.

## Time form redesign — in progress

Replace the shortcut inventory with Time basis and Window dropdowns. The absolute
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

Acceptance includes prefill without submission, draft restoration, dropdown and
mouse behavior, date/time/zone focus and visible cursor on small terminals, offset
normalization, nanosecond round trips, invalid-input rollback, rolling policy
persistence, stable selected anchors and timestamp-assistant revision fencing.
