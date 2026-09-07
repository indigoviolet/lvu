# lvu component model

Status: design specification for converting `crates/lvu` dialogs into owned
components. Companion to `module-partition.md` (why) and `dialog-system.md`
(what the components must be able to render). No Rust in this document has been
compiled; the sketches use today's real type names (`ViewState`, `QueryPurpose`,
`RowProvider`, `StorageRequest`, `DialogRegions`, `CursorBank`, …) so an
implementer can check them against `app.rs` line for line.

Measured starting point (main @ `9e65b35`): `app.rs` 11,302 lines, `ui.rs`
7,026; `App::handle` 3,502 lines, `App::handle_mouse` 1,904, `key_to_action`
550; `App` 74 fields; `Action` 180 variants; `HitRegions` 32 fields, 24 of them
belonging to one dialog each; 119 recursive `self.handle(Action::…)` calls
inside `handle`/`handle_mouse`. Fourteen `*DialogState` structs already hold
each dialog's data.

---

## 1. The component contract

```rust
// crates/lvu/src/component.rs  (new; ~150 lines, no dialog knowledge)

pub trait Component {
    /// Clickable things this component draws. `Copy` so the shell can hand it
    /// back inside a mouse event without borrowing the component.
    type Hit: Copy + Eq;

    /// Parameters needed to open (or re-open) this layer.
    type Open;

    /// Called by the shell when the layer is pushed. Seeds drafts from `ctx`
    /// (active view, selected row, clock). Never submits a query.
    fn open(&mut self, params: Self::Open, ctx: &mut Ctx<'_>);

    /// Input for this layer. Returns what the shell must do next.
    fn handle(&mut self, event: Event<Self::Hit>, ctx: &mut Ctx<'_>) -> Outcome;

    /// Draws the layer and records its own geometry (scroll offsets, hit rects,
    /// caret cell). `&mut self` because geometry is state: the same rects serve
    /// rendering, mouse hit-testing, scrolling and text selection (AGENTS.md).
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx<'_>) -> Surface;

    /// Resolve a screen point against the rects recorded by the last `render`.
    fn hit(&self, point: (u16, u16)) -> Option<Self::Hit>;

    /// The `Surface` the last `render` produced. The shell uses it for modal
    /// containment (§5.2) and selection bounds (§3) between frames.
    fn surface(&self) -> Surface;

    /// Palette entries this layer contributes (§4.3). Availability varies with
    /// component state, so this returns owned entries rather than a static
    /// slice, and it takes `&Views` rather than a `RenderCtx`: the palette is
    /// assembled where no provider or theme exists.
    fn commands(&self, views: &Views) -> Vec<CommandEntry> { Vec::new() }
}

/// Everything a component can receive. Raw keys, not `Action`s: the component
/// owns its keymap.
pub enum Event<H> {
    Key(KeyEvent),
    Paste(String),
    Mouse { kind: MouseEventKind, point: (u16, u16), hit: Option<H> },
    /// Escape or `q` reached this layer. The component closes its innermost
    /// popup and returns `Consumed`, or returns `Close`.
    Dismiss,
    /// A palette command the component declared in `commands()`.
    Command(CommandId),
    /// Something changed in the shared state this layer may be showing
    /// (§4.2). Delivered to every open layer, top first.
    View(ViewEvent),
    /// The terminal was resized; geometry from the last render is stale.
    Resize,
}

/// What the shell does after `handle`. Exactly one per event.
#[must_use]
pub enum Outcome {
    /// Not for me. The shell applies its global bindings (Ctrl-P, ?, Ctrl-L,
    /// Ctrl-C) or, for a mouse event, drops it. Nothing is redrawn.
    Ignored,
    /// Handled; redraw. Covers every internal state change, including a
    /// dropdown or completion popup closing on Dismiss.
    Consumed,
    /// Pop this layer. The shell restores whatever was under it.
    Close,
    /// Pop this layer and push another in the same tick (Bookmarks → Raw
    /// context; Recipes › history → Recipes with a selection).
    Replace(Open),
    /// Push a child layer on top of this one (dialog-system.md §10). The
    /// parent stays open and receives `View` events but no input.
    OpenChild(Open),
    /// Migration-only, and the mirror of `Action::Raw` (§6.4): pop this layer
    /// and hand a legacy dialog the `Action` that opens it. Needed whenever a
    /// converted layer opens a dialog that is converted later — Time's
    /// `🧠 Recognize timestamp` opens Ask, which is step 12. It becomes
    /// `Replace(Open::Ask { .. })` then, and the variant goes with the last
    /// legacy dialog.
    Legacy(Action),
}

/// Constructors for every layer the shell knows how to host. One variant per
/// dialog; grows by exactly one line per new dialog and nothing else.
pub enum Open {
    Storage,
    Time,
    Bookmarks,
    BookmarkNote { id: RowId },
    Fields,
    Context { anchor: RowId },
    Search, Advanced, Grouping,
    Enrichment,
    EnrichmentStep { stage: Option<EnrichmentDefinition> },
    ExternalCommand,
    Source { mode: SourceDialogMode },
    View, Recipes, RecipeHistory { recipe_id: String },
    Settings, Help, Ask { kind: AskAiKind }, Investigation,
}

/// Returned by `render`: the rect the shell needs for containment, the scrim,
/// text selection and the terminal caret. Nothing else leaves the component.
pub struct Surface {
    /// Everything the layer drew, including an anchored dropdown that extends
    /// past the dialog (§5.3). This is the rect §5.2 contains against; a rect
    /// that is only the dialog frame drops clicks on its own dropdown rows.
    pub popup: Rect,
    pub interior: Rect,       // `hit_regions.selection_modal`
    /// Where the terminal caret was drawn this frame, if it was drawn.
    pub caret: Option<(u16, u16)>,
    pub scrollable: bool,     // whether wheel events over `popup` are wanted
    /// Whether a text field has focus. Distinct from `caret`: a focused field
    /// scrolled out of a scrolling body draws no caret and still takes `q` as
    /// a character, so the `q`-dismissal rule reads this.
    pub text_focus: bool,
}
```

How the shell interprets each `Outcome`:

| Outcome | Shell action | Notes |
| --- | --- | --- |
| `Ignored` | Try the global keymap: Ctrl-C quit, Ctrl-L redraw, Ctrl-P palette, `?` help. Anything else is dropped. | A dialog never sees `q` as quit; dismissal is `Event::Dismiss`, produced by the shell from Esc (and from `q` only when no text field is focused, which the component reports via `Surface.text_focus` from its last render). |
| `Consumed` | Mark dirty. | The only way to say "redraw". |
| `Close` | Pop the layer; call nothing else on it. If the stack is now empty, base focus (`Selector`/`Logs`/`Details`) resumes exactly as before the first push. | Cleanup that needs the shell (cancel an in-flight scan, prune cursors) happens inside `handle` before returning `Close`, using `ctx`. |
| `Replace(open)` | Pop, then push `open` (§6.3). | Used instead of "return focus" fields such as `ContextDialogState::return_focus`, which disappear. |
| `OpenChild(open)` | Push `open` above the current top. | At most one child level; the shell rejects a second `OpenChild` from a layer that is already a child (debug assertion, then treated as `Replace`). |

Things deliberately **not** in `Outcome`, and where they went:

| Need | Mechanism |
| --- | --- |
| Request a query | `ctx.views.enqueue(...)` / `ctx.views.submit_capture_time(...)` — returns `Result<u64, SubmitRefused>` so the component words the refusal itself (§2.3). |
| Request background work (scan, save, agent) | The component owns an `Outbox<Req>`; `lvu-app` drains it and posts completions back (§2.4). |
| Request persistence | Nothing to request. Mutations through `ctx.views` bump `user_interaction_revision`; `lvu-app` already polls revisions. A component never saves. |
| Report a message | `ctx.notice(text)` sets the status-bar notice (today `action_notice`). Dialog-internal messages are component state rendered in the §7.4 message row. |
| Change focus / return focus | Stack discipline: `Close`, `Replace`, `OpenChild`. |
| Quit | Not a component's business. Ctrl-C is intercepted by the shell before dispatch. |

Why raw `KeyEvent`s instead of `Action`s: `key_to_action` exists only because the
keymap was separated from the state it depends on (`dialog.kind_dropdown`,
`dialog.focus == AskControl::Prompt`). Inside `handle` the component has that
state, so the 550-line function dissolves into one small `match` per component,
and `Action` stops needing `TimeInput(char)`, `SettingsInput(char)`,
`RecipeInput(char)`, … (eight char-input variants and eight backspace variants
today).

---

## 2. `Ctx`: the exact shared surface

### 2.1 Shape

```rust
/// Everything a component may touch while handling input. Built by the shell
/// from disjoint fields of `App` (§2.5); never from `&mut App`.
pub struct Ctx<'a> {
    pub views: &'a mut Views,                 // active view, drafts, query seam (from step 2)
    pub provider: &'a dyn RowProvider,        // read-only rows/pages/context
    pub cursors: &'a mut CursorBank,          // caret state for view-owned drafts
    pub clock: Clock,                         // now_unix_nanos, Instant::now()
    pub size: (u16, u16),                     // terminal size
    pub ascii: bool,                          // label fallback (🧠 → Agent)
    notices: &'a mut Notices,                 // via ctx.notice(); not pub
}

impl Ctx<'_> {
    pub fn notice(&mut self, text: impl Into<String>);
}

/// The read-only subset available during `render`.
pub struct RenderCtx<'a> {
    pub views: &'a Views,
    pub provider: &'a dyn RowProvider,
    pub theme: Theme,
    pub ascii: bool,
    pub size: (u16, u16),
    pub clock: Clock,
}
```

### 2.2 What is in `Ctx`, and what is not

| In `Ctx` | Why it is shared |
| --- | --- |
| `Views` — the map of `ViewState`, the active view id, the query queue and the completion routing | Every filter/enrichment/time/recipe dialog edits the active view's drafts and constraints; this **is** the product's shared state. |
| `RowProvider` (as `&dyn`) | Time seeds from the selected row; Fields, Context, Enrichment step read rows. Read-only by trait design. |
| `CursorBank` | Drafts that live in `ViewState` (search, advanced, enrichment, grouping) persist across dialog open/close; their carets must too. Dialog-owned fields keep their own `TextCursor` inside the component and do not use the bank. |
| Clock, terminal size, ASCII flag | Pure inputs. |
| Notices | The one-line status-bar message is a shell surface with one writer at a time. |

| Not in `Ctx` | Where instead |
| --- | --- |
| Other components (`storage_dialog`, `time_dialog`, …) | Never reachable. Cross-component effects go through `Views` (§4.2). |
| `focus`, the layer stack, `hit_regions` | Shell only (§3). |
| Theme *mutation* (Settings preview) | `Outcome`-free special case: Settings writes through `ctx.appearance: &mut Appearance` — a tiny struct `{ theme_id, delight, reduced_motion, ascii }` owned by the shell and added to `Ctx` **only** for Settings. It is the single exception to "no component-specific field in Ctx", recorded here so review can hold the line elsewhere. |
| Request queues of other features (`source_requests`, `recipe_requests`, …) | Each component owns its `Outbox` (§2.4). |
| `should_quit`, `show_startup_title`, `demo_mode` | Shell. |
| The `Frame` or `Buffer` outside `render` | Never. |

**As built (step 10): `ctx.appearance`.** The exception landed as
`component::Appearance { theme_id, delight_enabled, reduced_motion, ascii }`
with `Ctx.appearance: &mut Appearance`. Five deviations from the sketch, each
forced by preserving behaviour:

- **It is a top-level `App` field, not a member of `Shell`.** `shell_ctx` hands
  out `&mut Views` and `&mut Appearance` alongside `&mut Shell`; keeping them
  peers of `shell` is what makes the three borrows provably disjoint without
  splitting `Shell` itself. This is exactly how `Views` landed in step 2, and
  both move into `Shell` when the legacy fields are gone.
- **`Ctx.ascii` is now derived from `appearance.ascii`** rather than passed
  separately. It stays a `bool` copy, so a component that toggles ASCII sees the
  new value only on the next event; only Settings writes it, and it reads its
  own draft.
- **Settings reports `Surface.text_focus` for every focus except the open
  dropdown.** The field's name says "a text field has focus", but the rule it
  feeds is "does `q` dismiss". The legacy `Focus::Settings` key table mapped
  every bare character to `Action::SettingsInput`, which a non-text control
  silently dropped, so `q` never closed the dialog and closed the dropdown when
  one was open. Reporting `text_focus` from the dropdown state reproduces that
  exactly; reporting it from the focused control would make `q` on the Save
  button close the layer, which is a behaviour change.
- **The "settings are unavailable in this build" refusal is a guard in
  `App::push_layer`, not in the component.** `open()` returns nothing, so a
  component cannot decline its own push; and the refusal writes `source_notice`,
  which is not what `ctx.notice` (`action_notice`) writes. The component answers
  `is_configured()` and the shell does the rest in three lines. If a second
  layer needs this, `open` should gain a return value rather than the guard
  gaining arms.
- **`App::complete_settings_save` stays in the shell.** §2.4 has `lvu-app` call
  `app.layers.<x>.complete(...)` directly, and it does for the outbox
  (`app.layers.settings.outbox.take()`). But a successful save also sets the
  agent defaults (`ai_provider`/`ai_mode`/`ai_thinking`), the live `Appearance`
  and the status-line notice, none of which are the dialog's. The shell method
  keeps those and delegates the dialog's half — the generation fence, the draft,
  the status row — to `SettingsDialog::complete_save`/`fail_save`, which returns
  the appearance to show and the notice to write.

**Correction (step 10): dialog-owned carets are per field, not one.** §2.5 says
dialog-owned fields keep their own `TextCursor` inside the component, and step 2
found one cursor following focus was right for Time's segments. Settings is the
opposite case: its seven editable fields each had their own `CursorBank` slot
keyed by `settings:{generation}`, so a caret parked mid-value survived leaving
and re-entering the field. The component keeps `[Option<TextCursor>; 7]` indexed
by field, where `None` reproduces `CursorBank::get_or_end`'s "start at the end",
and `open()` clearing the array reproduces the identity change that bumping the
generation used to cause. `ctx.cursors` is unused here.

### 2.3 Reading the active view and submitting a query

`Views` is a new struct that takes over these `App` fields verbatim:
`view_states`, `query_requests`, `next_query_generation`, `selected_view`,
`views: Vec<ViewItem>`, `view_runtime_status`, plus the methods that only touch
them: `enqueue_query_value`, `enqueue_time_query`, `track_time_request`,
`submit_capture_time`, `apply_query_completion`, `apply_recipe_to_active_view`,
`applied_constraints`, `schedule_search`, `flush_debounced_searches`,
`refresh_rolling_capture_times`, `persistent_view_state`,
`restore_persistent_view*`, `view_interaction_revision`,
`view_definition_revision`. This is a move, not a rewrite; the bodies are
unchanged except that `self.view_states` becomes `self.states`.

**As built (step 2).** The extraction is done incrementally: a step moves in
only the fields the dialog it converts actually needs, so the churn lands with
the conversion that justifies it. Time moved in five fields — `items`,
`selected`, `states`, `requests`, `next_generation` — and two methods,
`submit_capture_time` and its private `enqueue_time_query`. Everything else in
the list above still lives on `App` and reaches those fields through
`#[doc(hidden)] pub(crate)` accessors, which shrink to nothing as the remaining
dialogs convert. It also owns `view_roles`, because the seam has to know whether an
edit may be applied in place at all. `view_runtime_status` has not moved: no
converted dialog reads it yet. The public surface a component may use is
therefore exactly `active_id`, `active`, `active_mut`, `role`,
`definition_is_fixed` and `submit_capture_time`; the by-id paths are the legacy
shell's.

**Fork staging is shell-owned, pending its own conversion.** `Views` knows that
a canonical view's definition is fixed and refuses an edit against it —
`submit_capture_time` returns `Err(SubmitRefused::DefinitionFixed)` — but it
does not create the derived view. `PendingFork`, `stage_fork`, `mark_fork_ready`
and the fork request queues stay on `App`, and a component hands the refusal
back with `Outcome::Legacy`. So `Views` is *not* the whole view lifecycle yet:
it is the query seam plus the roles that gate it. "All events is never filtered
in place" is enforced by the pair, and forking should migrate into `Views` as
its own step once its shape has settled — not inside a dialog conversion, which
is behaviour-preserving by rule and cannot absorb a subsystem that is still
changing.

```rust
pub struct Views {
    items: Vec<ViewItem>,
    selected: usize,
    states: HashMap<String, ViewState>,
    requests: HashMap<(String, QueryPurpose), QueryRequest>,
    next_generation: u64,
    runtime_status: HashMap<String, String>,
}

impl Views {
    pub fn active_id(&self) -> Option<&str>;
    pub fn active(&self) -> Option<&ViewState>;
    pub fn active_mut(&mut self) -> Option<&mut ViewState>;   // bumps nothing by itself
    pub fn touch(&mut self, view_id: &str);                   // user_interaction_revision += 1

    /// The one query seam. Reads and updates `desired_constraints`, records the
    /// pending editor state, inserts the `QueryRequest`. Returns the revision or
    /// `SubmitRefused` so the caller can keep its draft and word the message.
    pub fn enqueue(&mut self, view_id: &str, purpose: QueryPurpose, value: Option<String>)
        -> Result<u64, SubmitRefused>;
    pub fn submit_capture_time(&mut self, view_id: &str, window: Option<CaptureTimeRange>,
        policy: Option<CaptureTimePolicy>, basis: TimeBasis) -> Result<u64, SubmitRefused>;
    pub fn apply_recipe(&mut self, view_id: &str, config: RecipeConfig, now_nanos: i64)
        -> Result<u64, RecipeRejected>;

    /// Called by the shell when the worker answers. Mutates the view and
    /// returns what changed, for the shell to broadcast (§4.2).
    pub fn apply_completion(&mut self, completion: QueryCompletion) -> Vec<ViewEvent>;
}
```

A component submitting a time window, end to end, with no shell involvement:

```rust
impl Component for TimeDialog {
    fn handle(&mut self, event: Event<TimeHit>, ctx: &mut Ctx<'_>) -> Outcome {
        match event {
            Event::Key(k) if k.code == KeyCode::Enter && self.focus == TimeControl::Apply => {
                let Some(view_id) = ctx.views.active_id().map(str::to_owned) else {
                    return Outcome::Consumed;
                };
                // Drafts are per-view state, so they are read from the view, not
                // from `self`; an invalid draft leaves the applied view intact.
                let Some(state) = ctx.views.active_mut() else { return Outcome::Consumed };
                mark_time_edit(state);
                let parsed = parse_capture_range(&state.time_start_draft, &state.time_end_draft);
                match parsed {
                    Err(error) => { state.time_error = Some(error); Outcome::Consumed }
                    Ok(window) => match ctx.views.submit_capture_time(
                        &view_id, Some(window), Some(CaptureTimePolicy::Absolute(window)), self.basis,
                    ) {
                        Ok(_revision) => Outcome::Close,
                        // The view's definition is fixed: the edit becomes a
                        // derived view, staged by the shell (see below).
                        Err(SubmitRefused::DefinitionFixed) =>
                            Outcome::Legacy(Action::StageForkedTimeWindow),
                        Err(SubmitRefused::QueueFull) => {
                            self.message = Message::error("query queue is full · last window preserved");
                            Outcome::Consumed
                        }
                    },
                }
            }
            // …
        }
    }
}
```

Note the borrow sequencing: `state` (from `active_mut`) is used and dropped
before `submit_capture_time` borrows `ctx.views` again. The compiler enforces
it; nothing about it needs a `RefCell`.

### 2.4 Background work: the `Outbox`

Today `App` holds twelve `VecDeque<…Request>` fields and ten
`next_*_generation` counters, each drained by a `take_*` method that `lvu-app`
calls. Each moves into its component:

```rust
pub struct Outbox<Req> { queue: VecDeque<Req>, next_generation: u64, cap: usize }
impl<Req> Outbox<Req> {
    pub fn push(&mut self, req: Req) -> Result<(), OutboxFull>;
    pub fn next_generation(&mut self) -> u64;
    pub fn take(&mut self) -> Vec<Req>;                  // lvu-app calls this
}

pub struct StorageDialog {
    open: bool,
    generation: u64,
    snapshot: StorageSnapshot, selected: usize, scanning: bool, confirm_clear: bool,
    message: Message, geometry: StorageGeometry,          // from the last render
    pub outbox: Outbox<StorageRequest>,
}
impl StorageDialog {
    /// Completion from the worker. Ignored unless `generation` matches; that
    /// fence is the component's, exactly as `App::update_storage` does today.
    pub fn complete(&mut self, generation: u64, snapshot: StorageSnapshot, status: String, reviewed: bool) -> bool;
}
```

`lvu-app` changes `app.take_storage_requests()` to
`app.layers.storage.outbox.take()` and `app.update_storage(...)` to
`app.layers.storage.complete(...)`. The component slot is a **permanent field**,
not an `Option`, so a cancel pushed on close and a late completion after close
both have somewhere to land; `open: bool` (or a `Stage` enum) says whether the
layer is on the stack.

### 2.5 Borrow-checker realism

**The one hard rule: components and shared state must be different fields of
`App`.** `Ctx` cannot be produced by a method on `&mut App` while a component
inside `App` is also borrowed. The layout that makes it work:

```rust
pub struct App {
    pub shell: Shell,      // everything Ctx wraps + focus/stack/hit regions
    pub layers: Layers,    // one permanent slot per component
    // legacy fields remain here during migration (§6.4) and shrink to zero
}

pub struct Layers {
    pub storage: StorageDialog,
    pub time: TimeDialog,
    // … one per dialog, added as each is converted
    pub stack: Vec<LayerId>,          // bottom → top
}

impl App {
    pub fn handle_event<P: RowProvider + ?Sized>(&mut self, event: RawEvent, provider: &P) {
        let App { shell, layers, .. } = self;            // disjoint borrows
        let Some(top) = layers.stack.last().copied() else { return shell.handle_base(event, provider) };
        let mut ctx = shell.ctx(provider);               // borrows `shell` only
        let outcome = match top {
            LayerId::Storage => layers.storage.handle(event.for_layer(layers.storage.hit_at(event.point())), &mut ctx),
            LayerId::Time    => layers.time.handle(/* … */, &mut ctx),
            // one line per layer; bodies live in the component
        };
        drop(ctx);
        shell.apply(outcome, layers, provider);          // pop/push per §1 table
    }
}
```

Facts an implementer will hit, and the answer for each:

| Situation | Answer |
| --- | --- |
| `Ctx` needs `&mut Views` and the component needs `&mut self` | Different fields of `App` (`shell.views` vs `layers.storage`). Destructure `App` at the top of `handle_event`; do not call `self.method()` after that. |
| The shell must push a child while the parent is borrowed | The parent does not construct the child; it returns `OpenChild(Open::…)` with plain data. The shell, after `drop(ctx)`, calls `layers.enrichment_step.open(params, &mut ctx2)`. |
| `RowProvider` is a generic parameter on `handle` today | `Ctx` stores `&dyn RowProvider`. The trait is object-safe (no generics, no `Self` returns). `App::handle_event<P: ?Sized>` accepts `&P` and coerces. Test providers keep working. |
| `View` events must reach every open layer, not just the top | The shell iterates `layers.stack` and calls each layer's `handle(Event::View(e), ctx)` in turn; each call re-borrows `ctx` briefly. Outcomes other than `Consumed`/`Close` from a non-top layer are ignored (debug assertion). |
| `apply_query_completion` today mutates `time_dialog`, `enrichment_step`, `editor_completion` directly | Those writes become `ViewEvent`s returned from `Views::apply_completion` and consumed by the components (§4.2). The `close_enrichment_step` flag disappears. |
| Recursive `self.handle(Action::X)` (119 sites) | Inside a component they become private method calls (`self.clear(ctx)`). Across layers they are not allowed; that is what `Views` and `ViewEvent` are for. |
| Text editing shared helper (`apply_text_command`) borrows `text_cursors` and the draft's owner | Split by ownership: view-owned drafts use `ctx.cursors` + `ctx.views.active_mut()` sequentially; dialog-owned fields use a `TextField { value, cursor }` inside the component and `text_edit::edit` directly. **Correction (step 2):** the Time dialog's `segment_cursor` did *not* become a `TextField` per segment. It is one cursor that follows focus and resets to the end of whichever segment takes it; six independent cursors would remember a caret per segment, which is a behaviour change. It moved across unchanged, and Time uses no `ctx.cursors` at all. |
| Rendering needs `&App` for the base UI and `&mut Layers` for the layers | `ui::render_with_theme` takes `&mut App`; it destructures the same way, renders the base from `shell`, then each layer from `layers` with a `RenderCtx` built from `shell`. |

One compromise to state plainly: `Views` is a large object and `Ctx` hands out
`&mut Views` wholesale. A component *can* call `ctx.views.states_mut()` on a
view that is not active. The fix is API, not borrowing: `Views` exposes only
`active()/active_mut()` and by-id read access; by-id mutation exists solely for
`lvu-app` restoration paths and is `pub(crate)` on a separate `impl` block
under `#[doc(hidden)]`. Review checks that components never use it.

**As built (step 9): `Views::apply_recipe`.** The recipe seam landed as
specified — `apply_recipe(view_id, config, now_nanos) -> Result<u64,
RecipeRejected>` with `RecipeRejected::{InvalidStages, DefinitionFixed,
QueueFull}`, and the fixed-definition refusal handed back as
`Outcome::Legacy(Action::StageForkedRecipe(..))` exactly as Time hands back
`StageForkedTimeWindow`. What it needed underneath it, and why History is a
`Replace` rather than the child §6.3 called for, is recorded in §6.5.


---

## 3. What stays in the shell

Confirmed, with the reason each would produce spaghetti if pushed down:

| Shell-owned | Why |
| --- | --- |
| **Layer stack and base focus** (`Vec<LayerId>`, `Selector/Logs/Details`) | Dismissal order is a property of the *stack*, not of any layer: Esc goes to the top; `Close` pops; the layer underneath needs no notification. If layers owned focus they would have to know their neighbours. |
| **Modal containment and mouse routing** | The shell owns the only complete picture: palette open? which layer is on top? is the point inside `Surface.popup`? Only then does it ask the top layer `hit(point)` and deliver `Event::Mouse { hit }`. Clicks outside a modal are dropped by the shell; a component never sees them and cannot leak them to the log behind. |
| **Text-selection bounds and OSC 52 copy** | `terminal.rs` already uses `selection_modal`; it now reads `top.surface().interior`. |
| **Theme and appearance** | One writer (Settings, via the §2.2 exception), many readers through `RenderCtx.theme`. |
| **Terminal size, clock, tick** | Pure inputs. |
| **The query seam** (`Views`) | It is the product invariant carrier: last-applied view stays usable, drafts are separate from accepted constraints, completions are revision-fenced. Every dialog must go through the same code, so it must not be in any dialog. |
| **Notices** | One status line. |
| **Startup title, demo advance, quit** | Process lifecycle. |
| **The command palette overlay** | It sits above all layers and translates to `Event::Command` (§4.3); it is a shell overlay, not a layer, because it never takes focus away from the layer stack. |

Corrections to the brief:

- **Hit-region *storage* moves down; hit-region *routing* stays up.** The
  global `HitRegions` struct is what makes `handle_mouse` 1,904 lines: every
  dialog's rects are visible to every other dialog's arm. Each component records
  its own rects during `render` and answers `hit(point)`; the shell keeps only
  the base-UI rects (`log`, `log_rows`, `sidebar`, `sidebar_views`, `details`)
  and, per layer, the `Surface` returned by render. See §5.
- **Keymaps move down.** Focus-specific key tables are component state
  dependent (see §1); only the four global chords stay in the shell.
- **"Focus" inside a dialog is not shell focus.** Which control has focus within
  a dialog (`TimeControl`, `SettingsControl`, …) is component state and always
  was; the shell only knows which *layer* is on top.

---

## 4. Messages

### 4.1 Where `Action`'s 180 variants go

| Today | After | Count |
| --- | --- | --- |
| `OpenX` (Storage, Settings, Time, Recipes, Source, ViewDialog, FieldPicker, Bookmarks, Context, AskAi, Investigation, Search, Advanced, Enrichment, Grouping, CommandEnrichment, ToggleHelp) | `Action::Open(Open)` — one variant, payload enum from §1 | 17 → 1 |
| Per-dialog navigation/edit/activate (`MoveStorage`, `TimeMoveFocus`, `SettingsInput`, `RecipeBackspace`, `FocusAskControl`, …) | Deleted. Keys arrive raw; the component's `handle` matches them. | ≈120 → 0 |
| Shared text-edit verbs (`TextStartOfLine`, `TextMoveLeft`, `EditorPaste`, …) | Deleted from `Action`; `text_edit::EditCommand::from_key(KeyEvent) -> Option<EditCommand>` is a helper components call. | 12 → 0 |
| Modal scroll plumbing (`ScrollDialog`, `ModalVertical`, `ToggleDialogScrollFocus`, `ScrollHoveredDialog`, `ScrollDiscoveryStatus`, `TimeScroll`, `ScrollAskAi`, `ScrollSettingsDetails`, `ScrollHelp`, `ScrollInvestigation`) | Deleted. Each component owns its body scroll (`dialog_layout::DialogRegions.body_overflow` + its own offset). | 10 → 0 |
| Base UI (`MoveLine`, `MovePage`, `Top`, `End`, `ToggleFollow`, `NextView`, `CycleFocus`, `ToggleDetails`, `ScrollDetails`, `MoveHorizontal`, `ToggleBookmark`, `StopCapture`, `RestartCapture`, `ToggleExpandedGroup`, `Quit`, `Resize`, `FixtureAdvance`) | Stay as `Action`; they drive the shell's base surfaces until those become components too (out of scope). | ≈20 → 20 |
| `Mouse(MouseEvent)` | Deleted; the shell routes mouse events itself. | 1 → 0 |
| Palette-only intents that reach inside a dialog (`RefreshStorage`, `ClearStorage`, `SaveSettings`, `SubmitRecipe`, `NewInvestigation`, `AdaptRecipeSuggestion`, …) | `Action::Command(LayerId, CommandId)` delivered as `Event::Command` (§4.3). | ≈15 → 1 |

Target: `Action` ≈ 25 variants, all shell-level. A new dialog adds one `Open`
variant, one `LayerId`, one `Layers` field, one line in the render dispatch and
one in the input dispatch — and no `Action`.

### 4.2 One component's action affecting another: `Views` + `ViewEvent`

Applying a recipe changes search, advanced filter, enrichment chain, grouping,
time window, pins and colour at once. Today `apply_recipe_to_active_view` writes
all of that into `ViewState` and enqueues one query; the *dialogs* showing those
values are not involved because they read `ViewState` at render. That is already
the right shape, and it is why no component-to-component message is needed:

1. **Shared state is the medium.** Recipes calls `ctx.views.apply_recipe(...)`.
   Search/Advanced/Grouping/Time have no cached copy of the drafts; they render
   from `ctx.views.active()`. Rule: **a component may cache geometry and its own
   UI state, never a copy of `ViewState`.**
2. **Asynchronous consequences are broadcast as `ViewEvent`.** When the worker
   answers, `Views::apply_completion` returns:

   ```rust
   pub enum ViewEvent {
       QueryAccepted { view_id: String, purpose: QueryPurpose, revision: u64 },
       QueryRejected { view_id: String, purpose: QueryPurpose, message: String },
       TimeApplied { view_id: String },
       RecipeApplied { view_id: String },
       SourcesChanged { view_id: String },
       SelectionChanged { view_id: String },   // emitted by the shell's base UI
   }
   ```

   The shell delivers `Event::View(e)` to every open layer. The enrichment step
   layer returns `Close` on `QueryAccepted { purpose: Enrichment }` (replacing
   today's `close_enrichment_step` flag inside `apply_query_completion`); the
   Time dialog closes on `TimeApplied`; Search shows `● Applied` on
   `QueryAccepted { purpose: Search }`. `Views` never names a dialog.
3. **Synchronous consequences stay synchronous.** `apply_recipe` returns
   `Err(RecipeRejected)` for invalid stages; the Recipes component shows it in
   its own message row. Nothing about the accepted view changed.

The same pattern covers Fields (pin/colour → `ViewState.pinned_columns`,
`color_field` via `ctx.views.active_mut()`; the log re-renders from the view),
Bookmarks (`ViewState.bookmarks`), and Source (`ctx.sources`, see below).

Sources are the second shared aggregate. `sources: Vec<SourceItem>`,
`source_requests`, `source_controls`, `source_notice` move into a
`Sources` struct on `Shell` exposed through `ctx.sources` with `admit(...)`,
`stop(id)`, `restart(id)`. Only the Source dialog and the base sidebar use it.

### 4.3 The palette without dialog internals

`terminal.rs::palette_context` today peeks at `storage_dialog.confirm_clear`,
`recipe_dialog.mode`, `investigation_dialog.stage`, and `Command.action` is an
`Action`. Replace with:

```rust
pub struct CommandSpec {
    pub id: CommandId,               // component-scoped, e.g. StorageCmd::Refresh as u16
    pub name: &'static str, pub description: &'static str, pub category: &'static str,
    pub aliases: &'static [&'static str], pub shortcut: Option<&'static str>,
}
```

Each component's `commands(&self, views)` returns `Vec<CommandEntry>`: a
`CommandSpec` plus the component's current `unavailable_reason`, so the palette
can list an unavailable entry muted. Executing one yields
`Action::Command(LayerId, CommandId)`, which the shell converts to
`Event::Command(id)` for that layer. The palette stops knowing what a recipe
mode is.

Two corrections from the first two conversions:

- **Every slot contributes, not only the top layer.** A command that reaches
  inside a dialog is listed-but-muted from the base focus today (`Confirm
  derived-data cleanup` says `confirm cleanup in Storage preview first` from
  the log view). If only the open layer contributed, those entries would vanish
  from the catalog instead of greying out. The shell asks every component;
  each decides its own availability from its own `open` flag.
- **Entries are spliced beside the shell command that opens their layer**
  (`LayerId::palette_anchor`), so the catalog's grouping does not depend on
  which layer happens to be on the stack, and the shortcut column comes from
  the component rather than from probing a focus-specific key table.

---

## 5. Hit regions and layering

### 5.1 Component-owned geometry

```rust
/// Recorded by `render`, consumed by `hit()` and by the component's own
/// scroll/caret logic. Every rect here was painted this frame.
struct StorageGeometry {
    regions: DialogRegions,                 // from dialog_layout::regions
    rows: Vec<(Rect, usize)>,               // list rows → entry index
    actions: Vec<(Rect, StorageHit)>,       // buttons
    scrollbar: Option<Rect>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum StorageHit { Row(usize), Refresh, Cleanup, Scrollbar, Body }

fn hit(&self, point: (u16, u16)) -> Option<StorageHit> {
    let g = &self.geometry;
    g.actions.iter().find(|(r, _)| r.contains(point.into())).map(|(_, h)| *h)
        .or_else(|| g.rows.iter().find(|(r, _)| r.contains(point.into())).map(|(_, i)| StorageHit::Row(*i)))
        .or_else(|| g.regions.body.contains(point.into()).then_some(StorageHit::Body))
}
```

`HitRegions` shrinks to the base UI (`log`, `log_rows`, `log_row_indices`,
`sidebar`, `sidebar_views`, `details`) plus `selection_modal`, which the shell
sets from the top layer's `Surface.interior` after rendering. The 24
per-dialog vectors are deleted one conversion at a time.

### 5.2 Shell mouse routing (complete)

```
on mouse event at point p:
  if palette open                      → palette (unchanged)
  else if stack empty                  → base UI hit test (unchanged legacy code)
  else:
    top = layers.stack.last()
    surface = top.surface()            // cached from last render
    // `surface.popup` is everything the layer drew, dialog frame ∪ any open
    // anchored popup. Containing against the frame alone drops clicks on the
    // layer's own dropdown rows, which are allowed to extend past it (§5.3).
    if !surface.popup.contains(p):
        if kind is Down/Up/Drag        → drop (modal containment)
        if kind is Scroll              → drop (no scrolling the log behind a modal)
    else:
        hit = top.hit(p)
        deliver Event::Mouse { kind, point: p, hit } to top
```

Anchored popups (dropdowns, completion lists; `dialog_layout::anchored_rect`)
are drawn by the component that owns them and their rects are part of that
component's geometry, so a click on a dropdown row is simply
`Hit::WindowChoice(i)`. They are not layers.

### 5.3 Layering (dialog-system.md §10) in this model

- The stack is `Vec<LayerId>`; the shell renders bottom → top, applying
  `dialog_layout::scrim` before each layer, so a child dims its parent exactly
  once more than the base.
- A child is opened only by its parent returning `OpenChild(Open::…)`. The
  parent keeps its state (it is a permanent field) and keeps rendering under
  the scrim; while a child is on top the parent receives `Event::View` and
  `Event::Resize` but no key or mouse events.
- The child's title is a breadcrumb the *child* renders from a `parent_title`
  passed in its `Open` params (`Open::EnrichmentStep { stage, .. }` → the
  component knows it is `Enrichment › Edit step`).
- `Event::Dismiss` goes to the top only. The enrichment step's completion
  popup → `Consumed`; then the step layer → `Close`; then Enrichment → `Close`.
  This is today's `cancel_enrichment_step` chain expressed as three
  independent `handle` calls on three separate ticks, each with local knowledge.
- Compact terminals (`dialog_layout::is_compact`): the child's `render` gets
  the full frame `area` regardless; whether to draw the parent underneath is
  the shell's call (`if is_compact(area) { skip non-top layers }`).
- Containment for text selection: `selection_modal = top.surface().interior`.

---

## 6. Conversion order and migration

### 6.1 Pilot: Storage — done

Storage is the right first conversion because it exercises every part of the
contract that is *mechanical* and none that is *risky*:

| Contract element | Storage exercises it |
| --- | --- |
| Permanent slot + `open`/`Close` | Yes; open starts a scan, close pushes `Cancel` when scanning. |
| `Outbox` + completion fence | Yes: `StorageRequest {generation, kind}` and `update_storage(generation, …)` already exist; `lvu-app`'s `handle_storage` loop changes two call sites. |
| Component-owned keymap | `j/k/↑/↓`, `r`, `c` (twice for confirm), Esc. |
| Component-owned geometry + `hit()` | Rows, two buttons, scrollbar — and its render was already rewritten to `dialog_frame`/`pane` for `dialog-system.md`, so no layout work is mixed into the conversion. |
| Message row / dialog-system anatomy | Already rendered that way. |
| Palette command | `storage_confirmation_ready` in `palette_context` is exactly the kind of peek §4.3 removes. |
| Does **not** touch | `Views`, the query seam, `CursorBank`, child layers. The pilot proves the shell plumbing without the two hardest seams, so a failure in the pilot is a plumbing failure, not a product-invariant failure. |

Pilot commit contents: `component.rs` (trait, `Event`, `Outcome`, `Open`,
`Surface`, `Outbox`), `Shell`/`Layers` split of `App` with **legacy fields left
in place**, `StorageDialog` in `components/storage.rs` (state moved from
`StorageDialogState`, handlers moved from the four `Action::*Storage` arms, the
`Focus::Storage` blocks of `handle_mouse` and `key_to_action`, the
`CancelEditor` branch, and `render_storage`), deletion of those arms and of
`HitRegions::{storage_rows, storage_actions}`, `lvu-app` call-site change.
Acceptance: `test_shared_inspection_dialogs_pty.py` and `test:pty:matrix`
identical before and after; a new `TestBackend` test opens Storage through
`Action::Open(Open::Storage)`, clicks a row via `hit()`, and asserts
`selection_modal == surface.interior`.

### 6.2 Second: Time — done

Time is second because it is the first to cross the two hard seams — view-owned
drafts (`ViewState.time_*_draft`, fourteen fields) and `Views::submit_capture_time`
— while being self-contained (no child, no agent). It also carries the
`segment_cursor` text-editing special case, which becomes `TextField`s. If the
`Views` API is wrong, Time reveals it before eleven more dialogs depend on it.
Time's conversion is also when `Views` is extracted from `App` (§2.3); Storage
does not need it.

### 6.3 Order for the rest

| # | Layer | New seam it introduces |
| --- | --- | --- |
| 3 | Fields (`i`) | Provider reads (`row_by_id`), `ViewState.pinned_columns/color_field`; no outbox. |
| 4 | Raw context (`o`) | `Replace` semantics (it is opened from Bookmarks too); `context_page`; XL class. |
| 5 | Bookmarks (`B`) + Note child | **First `OpenChild`** (Note is a class-S child, and genuinely a second surface over the list it annotates); dialog-owned `TextField`. |
| 6 | Help (`?`) — done | Trivial; removes `show_help`, `help_scroll*`, `help_return_focus`. `help_return_focus` was the last dialog-owned copy of "where I came from", so retiring it is what forced the shell to keep the promise §1 already made: `pop_layer` restores the base focus the first push captured instead of assuming `Logs`. |
| 7 | Search, Advanced, Grouping (`/ p m`) — done | `ctx.cursors` for view-owned drafts; debounced `enqueue`; `ViewEvent::Query*` handling; the completion popup as component-owned geometry (removes `editor_completion` from `App`). |
| 8 | View (`v`) — done | `ViewMutationRequest` outbox; `ViewEvent::SourcesChanged`. Both arrived as specified; the three deviations it forced are recorded in §6.5. |
| 9 | Recipes (`r`) + History — done | `Views::apply_recipe`, `RecipeRequest` outbox with `RecipeRequestMeta` fences. History is reached and left by `Replace`, not `OpenChild`: this row said "History child" and was wrong (§6.5). |
| 10 | Settings (`,`) | The `ctx.appearance` exception; `SettingsRequest` outbox. |
| 11 | Source (`n`, three modes) — done | Four outboxes (`SourceLaunchRequest`, `DiscoveryUiRequest`, `PathCompletionRequest`, `SourceAiRequest`) folded into one `SourceRequest` enum, drained by kind (§8). `ctx.sources` stayed read-only: there was no mutating half to add (§6.5). |
| 12 | Ask 🧠, Investigation 🧠 | Agent outboxes; multi-line `TextField`; long-running stages. |
| 13 | Enrichment + Step child + External command | Last, and only after the in-flight two-layer work lands: it is the deepest stack and has the most `ViewEvent` handling. Its `Focus::EnrichmentEditor`/`EnrichmentStep`/`CommandEnrichment` trio maps to `LayerId::Enrichment`, `EnrichmentStep`, `ExternalCommand`. The step editor is the model's one real `OpenChild`; External command is a `Replace`, because it is not a child today (§6.5). |

Each step is one commit, deletes its `Action` variants, `Focus` variant,
`HitRegions` vectors, `handle`/`handle_mouse`/`key_to_action` arms and
`render_*` dispatch line, and moves (not rewrites) the bodies. The PTY matrix
must be green after each.

### 6.4 Hosting converted and unconverted dialogs together

During migration `App` has three regions: `shell`, `layers`, and the legacy
fields. The bridge is small and mechanical:

```rust
pub enum Focus {
    Selector, Logs, Details,
    Layer,                       // a converted component is on top; see layers.stack
    // legacy variants deleted one per conversion:
    SearchEditor, AdvancedEditor, /* … */
}

impl App {
    pub fn handle<P: RowProvider + ?Sized>(&mut self, action: Action, provider: &P) {
        match action {
            Action::Open(open) => self.push_layer(open, provider),          // §6.3 converted layers
            Action::Command(layer, id) => self.deliver(layer, Event::Command(id), provider),
            Action::Raw(event) if self.focus == Focus::Layer => self.handle_event(event, provider),
            // legacy path: unchanged 3,500-line match, minus deleted arms
            other => self.handle_legacy(other, provider),
        }
    }
}
```

- **Input:** `terminal.rs` produces `Action::Raw(RawEvent)` when
  `app.focus == Focus::Layer`; otherwise `app.key_to_action(key)` as today.
  `Action::Raw` is a migration-only variant deleted with the last legacy focus.
- **Opening:** legacy `Action::OpenStorage` is deleted in the pilot; the key
  binding in the base keymap becomes `Action::Open(Open::Storage)`, and the
  palette entry points at it. Legacy `Open*` variants for unconverted dialogs
  stay until their turn.
- **Layer over legacy dialog:** not allowed. A converted layer may be pushed
  only when `focus` is a base focus or `Focus::Layer`. Legacy dialogs that open
  converted ones (Bookmarks → Context before Bookmarks is converted) keep their
  legacy path until both are converted; the order in §6.3 is chosen so every
  parent is converted no later than its child.
- **Rendering:** `ui::render_with_theme` renders the base, then legacy dialog
  `render_*` for a legacy focus, **or** the layer stack for `Focus::Layer`.
  Never both.
- **Dismissal:** `is_layer_dismissal_key` in `terminal.rs` is unchanged; for
  `Focus::Layer` it becomes `Event::Dismiss` to the top layer.
- **Completion routing:** while `Views` still lives in `App` (before step 2),
  `apply_query_completion` stays as is. From step 2, it moves to `Views` and
  the shell broadcasts `ViewEvent`s; legacy dialogs that used to be closed from
  inside `apply_query_completion` keep a two-line legacy shim in the shell that
  reacts to the same `ViewEvent` (`time_dialog = None` on `TimeApplied`) until
  they are converted.

Nothing in the bridge is clever; that is the point. Every step is a move with a
compiler-checked boundary at the end.


### 6.5 Deviations recorded by the conversions so far

Each of these is a place the built shell differs from §1–§5. They are listed
here rather than edited silently into the sketches above, so a reviewer can see
what the conversions actually forced.

**Step 6 (Help): `pop_layer` restores the base focus the first push captured.**
§1 says base focus "resumes exactly as before the first push"; the pilot's
`pop_layer` restored `Focus::Logs` unconditionally, which was invisible until
Help — the one dialog with a `help_return_focus` field — was converted. The
stack now records the base focus on the first push. A legacy dialog focus is
never recorded: §6.4 forbids pushing a layer over one, and it is not a state the
stack may return to.

**Step 8 (View): `Ctx.sources` / `RenderCtx.sources` are a read-only slice.**
§4.2 plans a `Sources` struct on `Shell` with `admit`/`stop`/`restart`, reached
through `ctx.sources`. Membership editing needs only the read half — the open
sources by name and order — and the mutating half belongs with the Source
dialog, which is step 11. Adding the slice now and widening it to the struct
then keeps this conversion behaviour-preserving; adding the whole aggregate
would drag an unconverted subsystem into a conversion commit, which §2.3 already
rejected for fork staging. This is not an anti-pattern #2 exception: two
components and the base sidebar read it.

**Step 8: `Open::needs_active_view()`.** `Action::OpenViewDialog` opened nothing
when the view list was empty, and every other `Open*` arm carried its own
precondition the same way. `push_layer` is otherwise unconditional, so the
precondition became one line of data on `Open` beside `layer()`. It stays out of
`App::handle`, which remains routing-only (§7.7). Only `Open::View` answers
`true`: Time seeds from the active view but opened without one before its
conversion, and Storage and Help never read views.

**Step 9 (Recipes): §6.3 row 9 said "History child" and was wrong; §1 was
right.** §1's `Outcome::Replace` doc-comment already gave "Recipes › history →
Recipes with a selection" as its own example, and that is what shipped:
`Replace` in both directions, `Alt-S`/`I`/`E`/`U` out of History as
`Replace(Open::Recipes { mode })`, and `Alt-H` into it as
`Replace(Open::RecipeHistory { .. })`. Row 9 has been corrected and the first
`OpenChild` is now step 5, Bookmarks' Note, which is a real second surface over
the list it annotates.

The reason is that `OpenChild` is not free, and every cost it carries here was a
*behaviour change* in a commit whose rule is that there are none. A child
renders over its scrimmed parent (§5.3), and the two popups are the same class
with content-driven heights — Browse draws five action buttons, History one — so
the parent's border shows around the child. Escape would pop one layer instead
of closing the dialog. `Alt-B` would close onto a list instead of re-fetching
one. Applying a revision would need a second pop that `Close` cannot express.
And a mode entered from History — `Alt-E` exports the *selected revision*,
`Alt-U` updates it — would act on the parent's browse selection instead, because
a second slot cannot see the child's list. A conversion may not trade any of
that away; a visible delta is a stop-and-ask, never a trade.

**Step 9: two `LayerId`s over one slot.** `LayerId::Recipes` and
`LayerId::RecipeHistory` are two surfaces — different title, heading and
actions, and the stack says which one is showing — but they share one
`Layers.recipes`. That is the direct consequence of the paragraph above: the
list, the selection, the name field and the `RecipeRequestMeta` generation are
one dialog's, and every transition is a `Replace` that must carry them across
unchanged. A second slot would have to copy them, and copying is how the
behaviour changes above creep back in. `Open::RecipeHistory` therefore carries
only what the surface needs that the state does not already hold: the recipe id
to ask about and the name to put in the breadcrumb.

**Step 9: `enqueue_query_value` moved into `Views`.** `Views::apply_recipe`
needs the Advanced enqueue *including* its editor bookkeeping — it reads
`advanced.pending_generation` back immediately — and duplicating that would have
made a second evaluator. The body moved down as `Views::enqueue_value`;
`App::enqueue_query_value` keeps only the fixed-definition fork guard and
delegates. Step 7 inherits the seam already in place.

**Step 9: `Views::applied_recipe_config` is not `App::persistent_view_state`.**
Save and Update need the view's accepted definition and presentation.
`persistent_view_state` also carries bookmarks and the selection stamp, both
shell-owned and neither part of a recipe, so the projection the component needs
is its own read-only method on `Views` rather than a move of a method that
cannot follow. `Views::active_source_id` arrived for the same reason: a recipe
outcome is keyed by the source it was judged on.

**Step 8: `ViewEvent::SourcesChanged` is broadcast for all four view modes.**
The success path (`App::view_request_succeeded`) is called by `lvu-app` with the
view id alone, and every mode ends with that view's membership newly established
or re-established — Blank and Clone register a view with its source set, Sources
edits it, Rename re-affirms it. Splitting Rename off would mean a second event
whose only consumer is the dialog that is closing, which is what §5 forbids:
events describe what happened to the view, not what a dialog should do. The
component decides; the shell does not null a dialog field any more.

**Step 7 (editors): `editor_completion` stays on `App`, and its content and
its drawing become shared helpers.** §6.3's note that step 7 "removes
`editor_completion` from `App`" assumed the completion popup belonged to the
Advanced filter alone. It does not: the enrichment *step* editor offers the
same popup, and Enrichment is step 13. So the Advanced layer owns its own
`EditorCompletionState` and its own row rects, `App` keeps its copy and its
`HitRegions::editor_completion_rows` for the enrichment step, and the two share
one implementation of what a completion *is* — `app::sample_editor_completion`
for the items and the status, `ui::draw_editor_completion` for the popup. Two
owners of the same shape, not two implementations of it. `App`'s copy, the
three `Action::*EditorCompletion` variants and the `HitRegions` vector all go
with step 13.

**Step 7: one evaluator, three guard-and-refuse wrappers.** Steps 7 and 9 both
lifted `App::enqueue_query_value` into `Views` in flight, under different names
and return types. Reconciled to Recipes' shape: `Views::enqueue_value` is the
single evaluator — `pub(crate)`, `Option<u64>`, the moved body — and the three
public seams are thin wrappers that check the view's role and word the refusal
their caller has to act on. `submit_capture_time`, `apply_recipe` and `enqueue`
now read the same way, and `apply_recipe_in_place` exists for exactly the reason
`enqueue_value` does: a fork candidate is derived, so re-asking the
fixed-definition question would be misleading. `App::enqueue_query_value` keeps
only its own fork guard and calls the evaluator directly. `enqueue` is what a
component uses, because it is the one that can say `DefinitionFixed` — a full
queue keeps the draft and is worded in the editor, a fixed definition becomes a
derived view.

**Step 7: the editors need no `ViewEvent`.** §6.3 lists `ViewEvent::Query*`
handling for this step, on the strength of §4.2's example ("Search shows
`● Applied` on `QueryAccepted`"). In the built shell they do not: the accepted
value, the error and the pending fence are `ViewState` fields, the layer renders
from `ctx.views.active()` every frame, and `App::apply_query_completion` already
writes them. An event would be a second path to the same screen. What the
editors *do* take is `SourcesChanged`, which invalidates a completion popup
sampled from rows the view no longer has. `ViewEvent::Query*` is still the right
shape for a layer that must react rather than re-read — the enrichment step
closing on an accepted save is the real case, and it arrives with step 13.

**Step 7: `RenderCtx.cursors`.** The three editors are the first layers whose
draft is *view*-owned, so their caret is the `CursorBank`'s rather than the
component's, and `render` has to read it. `Ctx` hands out `&mut CursorBank`;
`RenderCtx` now carries `&CursorBank` and `CursorBank::peek` reads a clamped
caret without touching the bank's LRU order. Every remaining view-owned-draft
dialog needs this, so it is not a one-component field (§7.2).

**Step 7: `Surface::text_focus` is derived, not recorded.** §1 says the shell
reads `text_focus` "from its last render". For a layer whose whole content is a
text field that is wrong in a way nothing before it exposed: between `open` and
the first frame the flag would be `false`, and the first `q` the user typed
would dismiss the dialog instead of appearing in it. The editors override
`Component::surface` to compute it from state; everything else in `Surface`
stays geometry from the last render.

**Step 7: a palette row a layer and a legacy dialog can both reach.**
`Complete editor field or value` is available from the Advanced layer *and*
from the enrichment editor's focus. Splicing the component's entry beside the
static one would list it twice, which §4.3's catalog test forbids, and deleting
either half would silently narrow where the command is offered. The splice now
*takes over* an entry whose id the catalog already lists when the layer reports
it available, so one row routes to whichever owner can currently serve it. It
becomes an ordinary component entry when Enrichment converts.

**Step 8: `component::NoRows`.** `App::view_request_succeeded` runs in
`lvu-app`'s request loop, which holds no `RowProvider`, and a `ViewEvent` still
has to reach every open layer. `NoRows` is the empty provider the shell builds a
`Ctx` from there. §4.2 already says a layer decides what a view event means from
`Views` and never from rows, so serving none is the contract rather than a
shortcut — a layer that reached for a row on a view event would be the bug the
rule already forbids.

**Step 11 (Source): `ctx.sources` gained no mutating half, because Source has
none.** §4.2 plans `admit`/`stop`/`restart` on a `Sources` struct reached
through `ctx.sources`, and §6.5's step-8 note deferred the mutating half to
this step. Building it showed the half does not exist. `admit` is the
component's own `SourceRequest::Launch`, which goes through its outbox like
every other request a component makes; `stop` and `restart` are
`Action::StopCapture`/`RestartCapture` under `Focus::Logs`/`Selector`, so they
belong to the base sidebar, which is shell code and not a component (§8 leaves
base surfaces out of the model). The Source dialog reads nothing from
`app.sources` either. Adding the struct now would mean a seam whose `admit`
duplicates an outbox and whose `stop`/`restart` no component calls — anti-pattern
#2 in spirit. `Ctx.sources` therefore stays the read-only slice step 8 added,
and the plan in §4.2 should be read as describing the sidebar's future
conversion rather than this one.

**Step 11: one `Outbox<SourceRequest>`, four typed drains.** §8 leaves open
whether `lvu-app` merges its four Source loops or keeps four drains of one
queue. It keeps four: a path scan is debounced, a discovery scan is
cancellable, and the agent runs a session on a different schedule, so merging
them would have meant one drain point stashing three kinds of leftovers.
`Outbox::take_where` drains the requests one consumer recognises and leaves the
rest queued, preserving order within a kind, and each kind still refuses at its
own depth rather than at the shared cap.

**Step 11: the path-completion debounce moved with the requests it gates.**
`App::take_path_completion_requests` withheld a scan until
`SOURCE_PATH_COMPLETION_DEBOUNCE` had passed. That is not `Outbox` behaviour and
it is not a `Clock` reading either — it is `Instant::now()`, as it was on `App`
— so it lives in `SourceDialog::take_path_completions`, which is the one drain
that gates itself. Automatic completion asking on every keystroke is what the
PTY suite checks, so the gate had to move whole rather than be re-derived.

**Step 11: `Ctx.agent` is the second documented exception to anti-pattern #2.**
`SourceAiRequest::Start` stamps the provider, mode and thinking level the
running settings resolved. They were `App::ai_provider`/`ai_mode`/`ai_thinking`,
written only by `App::configure_ai` and by a settings save — shell
configuration, exactly like `appearance`, and not component state by any
reading. Ask and Investigation (step 12) read the same three. They are now
`component::AgentDefaults` behind a read-only `Ctx.agent`.

**Step 11: `Surface.text_focus` is seeded when a layer opens.** §1 documents it
as coming from the last render, and the shell reads it to decide whether `q`
dismisses. A layer that opens straight onto a text field and receives a key
before its first frame would take that `q` as a dismissal, which
`is_text_editing` never did. In the running app a frame always intervenes;
`App::new` on an empty workspace — which opens Add source before any event — is
the case where one does not, so `SourceDialog::open` seeds the flag. The other
layers have the same latent gap and no way to reach it; if a third one opens on
a field, seeding should move into the shell rather than be repeated.

**Step 11: `Action::CompleteSourcePath` and `Action::ToggleSourceKind` had no
producer.** Neither was bound to a key or offered by the palette; only tests
drove them. Automatic completion is scheduled by typing and accepted by Enter,
which is what the PTY suite exercises, so both variants went with the other
fourteen and the tests that used them now drive the reachable path.
**Step 13: the step editor is the model's only true child; External command is
not one.** §6.3 and dialog-system.md §10 both listed "External command under
Enrichment" as a child. It never was: `render_command_enrichment` drew no parent
behind it, and its Escape set `Focus::Logs`, not `Focus::EnrichmentEditor`.
Making it a child would have put a scrimmed step list behind it and changed
where Escape lands — a visible delta, which a conversion may not trade for
tidiness. So Enrichment reaches it with `Outcome::Replace(Open::ExternalCommand)`
and it closes to the base. The step editor, by contrast, already drew its parent
scrimmed and inset itself to `parent.width - 4`, which is exactly §5.3 and
exactly what the shell's stack loop does, so `OpenChild` is delta-free there.
dialog-system.md §10's list is corrected to match.

**Step 13: `RenderCtx.active` tells a layer whether it is the top of the stack.**
§10 says a parent under a child keeps its frame and title but drops to `border`
colour. `render_enrichment_step_list` took an `active: bool` and, when false,
drew the frame and returned — the body, message row and buttons are not painted
under a child at all. Only the parent knows how much of itself that leaves
drawable, and repainting the border afterwards would not reproduce it, so the
shell passes `is_top` in `RenderCtx` rather than trying to derive it. The other
layers ignore it.

**Step 13: `close_enrichment_step` became `ViewEvent::QueryAccepted`, emitted
only where the flag was set.** `App::apply_query_completion` reached into
`self.enrichment_step` to close the editor when a step was accepted. That is
§4.2's event, so the shell now broadcasts `QueryAccepted { purpose: Enrichment }`
and the step layer decides for itself, fenced on the view it holds. The event is
emitted at exactly the point the flag was set — an accepted Add or Edit of the
draft the editor owns — rather than for every accepted query: a Reaffirm after a
failure, or a Remove, must leave an open editor alone, and the layer cannot tell
those apart from `Views` alone. Emitting the other purposes is step 7's to do if
its layers ever need them.

**Step 13: the shared editor completion finished its move out of `App`.** W18
left `editor_completion`, its generation counter, the three
`Action::*EditorCompletion` variants and `HitRegions::editor_completion_rows` on
the shell because the enrichment step editor was still legacy (§6.5, step 7).
It was the last consumer, so all of it is gone; the step layer owns its own
`EditorCompletionState` with the same implementation `components::editors` uses,
including the eight-row popup window that differs from the editors' simpler one.

**Step 13: `Views::enqueue_enrichment_chain` is the seam for a whole chain.**
`Views::enqueue_value` submits one added or edited step. A removal or a reorder
replaces the chain wholesale and carries a `PendingEnrichmentMutation` the
completion path reads, so `App::enqueue_enrichment_chain` moved to `Views`
verbatim beside it. Both enrichment layers submit through it, and the shell's
own reaffirm-after-failure path still uses it unchanged.

**Step 13: the command dialog's completion paths stayed as `App` forwarders.**
`finish_command_enrichment_save`/`_review`/`_run` and `begin_command_result_save`
are `lvu-app`'s API and they fence against both `Views` and the dialog. The
dialog, its outbox and its two pending-generation maps are the layer's; the four
methods are three-line forwarders that hand the layer `&mut Views` and the
notice slot. This is the shape Storage and Source already use.

---

## 7. Anti-patterns (review checklist)

Each of these is a concrete regression toward the god object. Reject the diff.

1. **A component method takes `&mut App` or `&App`.** Components take `Ctx`,
   `RenderCtx`, or nothing.
2. **A new field on `Ctx` or `RenderCtx` that only one component reads.** The
   documented exceptions are `appearance` for Settings and `agent` for Source,
   Ask and Investigation; both are shell configuration with one writer, and both
   are argued in §6.5. A third needs a paragraph there explaining why it is not
   component state.
3. **`Views`, `Sources` or `ViewState` gains a field that is not persisted per
   view or per source.** UI-only state (which control has focus, scroll
   offsets, dropdown open, confirm pending) belongs in the component. Existing
   `ViewState.time_*_draft`, `enrichment_control`, `field_picker_*` fields are
   accepted debt: they persist drafts across dialog open/close; any new one must
   justify itself the same way.
4. **`Outcome` or `Open` carries a closure, a `Box<dyn Any>`, or a reference to
   another component.** Payloads are plain data.
5. **`ViewEvent` names a dialog** (`CloseTimeDialog`). Events describe what
   happened to the view; components decide what to do.
6. **A new `Action` variant whose handling depends on which dialog is open.**
   That is a component keymap entry or a `CommandSpec`.
7. **A `match` arm in `App::handle_event`, `App::render`, or the mouse router
   with a body longer than one call.** Routing only.
8. **`pub` fields on a component read by `ui.rs` or `terminal.rs`.** The only
   public surface is `Component` plus `outbox`/`complete` for `lvu-app`.
9. **Geometry computed in two places.** If `hit()` tests a rect that `render`
   did not store this frame, or `terminal.rs` computes a modal bound itself,
   the shared-geometry invariant is broken.
10. **A component caches rows or `ViewState` between frames.** Read through
    `ctx` every time; the provider is bounded and non-blocking by contract.
11. **A component calls another component's `complete`, `open`, or reads its
    `outbox`.** Cross-effects go through `Views`/`Sources`/`ViewEvent`.
12. **`HitRegions` gains a field.** It only loses them.
13. **A conversion commit changes behaviour.** Same PTY matrix output before
    and after; presentation changes land separately under `dialog-system.md`.
14. **`Focus::Layer` drives behaviour anywhere except the four bridge sites**
    (input dispatch in `terminal.rs`, the `Action::Raw` guard in `App::handle`,
    render dispatch in `ui.rs`, and the early return in the free
    `key_to_action` that stops the base key table leaking into a layer's
    shortcut column). Inert arms in exhaustive `match self.focus` expressions
    do not count; they are unavoidable while `Focus` still names legacy
    dialogs, and they disappear with it.

---

## 8. Not resolved here

- **Base surfaces as components.** Logs, sidebar and Details stay in the shell.
  They are the natural next step (`LayerId` becomes a general `Surface` id with
  the base as layer 0), but they own scrolling, follow mode and selection that
  `Views` also touches; that boundary deserves its own spec after the dialogs
  are done.
- **`Views` mutability width.** §2.5 states the compromise: `&mut Views` is
  broader than any one component needs. If review pressure demands it, the
  next narrowing is a `ViewHandle<'a>` bound to the active view id that exposes
  only `state()`, `state_mut()`, `enqueue(purpose, value)`,
  `submit_capture_time(...)`. Do this only after two or three conversions show
  which methods components actually call.
- **Multiple simultaneous outboxes for Source.** Whether `lvu-app`'s four
  Source-related loops merge into one worker or stay four drains of one
  `Outbox<SourceRequest>` is an `lvu-app` decision; the component side is the
  same either way.
- **Where `text_edit::EditCommand::from_key` lives** and whether Ctrl-A/E/K stay
  universal or become per-field policy (`EditPolicy` already exists). Not a
  component-model question; decide in step 7.
