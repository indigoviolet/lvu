//! The component contract (`docs/component-model.md` §1, §2, §5).
//!
//! A component owns its state, its keymap, its geometry and its background
//! requests. It receives raw input and a `Ctx` built from disjoint fields of
//! `App`; it never sees `App` itself, so the boundary is compiler-enforced.
//!
//! Migration status: `Ctx` carries `views` (extracted from `App` with the Time
//! conversion, §6.2) and `appearance` (added with Settings, §6.3 step 10).

use std::collections::VecDeque;

use crossterm::event::{KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::{Frame, layout::Rect};

use crate::app::{EnrichmentStageId, QueryPurpose, RecipeDialogMode, SourceItem, Views};
use crate::command_palette::CommandId;
use crate::provider::{DisplayRow, RowId, RowPage, RowProvider};
use crate::text_edit::CursorBank;
use crate::theme::{Theme, ThemeId};

/// The shell's appearance state, and the one documented exception to "no
/// component-specific field in `Ctx`" (§2.2): Settings previews a draft theme,
/// delight, reduced-motion or ASCII choice by writing it live and rolls it back
/// from its own saved baseline on dismissal. Every other reader takes it
/// through `RenderCtx.theme` / `RenderCtx.ascii`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Appearance {
    pub theme_id: ThemeId,
    pub delight_enabled: bool,
    pub reduced_motion: bool,
    pub ascii: bool,
    /// Fixed UTC offset the log viewport formats timestamps in, as a token
    /// (`"Z"`, `"+02:00"`). It belongs to the reader, not to a view: two views
    /// of one source disagreeing about what `14:30` means would be worse than
    /// having to set it once (§2.3 exception for `appearance`).
    pub display_zone: String,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            theme_id: ThemeId::Terminal,
            delight_enabled: std::env::var_os("LVU_NO_DELIGHT").is_none(),
            reduced_motion: std::env::var_os("LVU_REDUCED_MOTION").is_some(),
            ascii: std::env::var_os("LVU_ASCII").is_some(),
            display_zone: crate::app::DEFAULT_DISPLAY_ZONE.to_owned(),
        }
    }
}

/// One per converted dialog. The shell stacks these, not the components.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LayerId {
    Storage,
    Time,
    Help,
    Settings,
    Fields,
    View,
    Source,
    /// The per-view folding policy: which column runs fold on, the minimum
    /// run, the scope, and the on/off toggle.
    Folding,
    Recipes,
    /// The revision list. Its own layer and its own surface — title, heading
    /// and actions all differ — but the same slot as `Recipes`, because every
    /// transition between them is a `Replace` that must carry the list, the
    /// selection, the name field and the fence id across (§6.5).
    RecipeHistory,
    /// The Filter dialog: Search and Advanced are its two tabs (§12.1), one
    /// layer and one slot; `Open::Search`/`Open::Advanced` pick the tab.
    Filter,
    Grouping,
    /// The enrichment step list.
    Enrichment,
    /// The step editor. A true child of `Enrichment` (§5.3): saving or
    /// cancelling a step returns the user to the list it was opened from, and
    /// the list is already drawn scrimmed behind it.
    EnrichmentStep,
    /// Command enrichment. Its own layer rather than a child of `Enrichment`
    /// (§6.5), because it neither draws the list behind it nor returns to it.
    ExternalCommand,
    Bookmarks,
    Ask,
    Investigation,
}

/// Constructors for every layer the shell knows how to host (§1). Grows by
/// exactly one variant per conversion.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Open {
    Storage,
    Time,
    Help,
    Settings,
    Fields,
    View,
    Source,
    Folding,
    /// The Recipes layer in one of its editable or browsing modes. Reaching a
    /// mode is a `Replace` from History and a plain state change from within
    /// Recipes, so the mode travels as `Open` data (§6.5).
    Recipes {
        mode: RecipeDialogMode,
    },
    /// The revision list for one recipe, reached by `Replace` from Recipes and
    /// left the same way. `recipe_name` is the breadcrumb the layer renders
    /// from its `Open` params.
    RecipeHistory {
        recipe_id: String,
        recipe_name: String,
    },
    /// The Filter dialog on its Search tab (§12.1). `/` opens this.
    Search,
    /// The same Filter dialog on its Advanced tab; reached from the palette
    /// row, from an accepted 🧠 filter proposal, or by switching tabs inside.
    Advanced,
    Grouping,
    Enrichment,
    /// A new step when `editing` is `None`, otherwise the existing stage. The
    /// list decides which before it opens the child, so the child never has to
    /// read the parent back.
    ///
    /// `prefill` seeds the expression the editor opens on. It exists because
    /// Folding's `[ New column… ]` builds a concatenation of the fields the
    /// user picked and hands it here rather than growing a second
    /// field-combination mechanism of its own; the user still reviews and saves
    /// it as an ordinary step. `None` resumes whatever draft the view holds.
    EnrichmentStep {
        editing: Option<EnrichmentStageId>,
        prefill: Option<String>,
    },
    /// A command step (§12.6): `stage` edits an existing step, `None` adds a
    /// new one inserted at `insert_at` in the chain (clamped to the end).
    ExternalCommand {
        stage: Option<EnrichmentStageId>,
        insert_at: usize,
    },
    Bookmarks,
    Ask(crate::components::ask::AskOpen),
    Investigation,
}

impl LayerId {
    /// Where this layer's contributed palette entries belong: immediately
    /// after the shell command that opens the layer, so the palette's grouping
    /// does not depend on which layer happens to be on the stack.
    pub fn palette_anchor(self) -> CommandId {
        match self {
            LayerId::Storage => CommandId::StoragePreview,
            LayerId::Time => CommandId::TimeWindow,
            LayerId::Help => CommandId::Help,
            LayerId::Settings => CommandId::Settings,
            LayerId::Fields => CommandId::Fields,
            LayerId::View => CommandId::ViewDialog,
            LayerId::Source => CommandId::AddSource,
            LayerId::Folding => CommandId::FoldingDialog,
            LayerId::Recipes | LayerId::RecipeHistory => CommandId::Recipes,
            LayerId::Filter => CommandId::LiteralFilter,
            LayerId::Grouping => CommandId::Grouping,
            LayerId::Enrichment | LayerId::EnrichmentStep => CommandId::Enrichment,
            LayerId::ExternalCommand => CommandId::CommandEnrichment,
            LayerId::Bookmarks => CommandId::Bookmarks,
            LayerId::Ask => CommandId::AskAi,
            LayerId::Investigation => CommandId::Investigations,
        }
    }
}

impl Open {
    pub fn layer(&self) -> LayerId {
        match self {
            Open::Storage => LayerId::Storage,
            Open::Time => LayerId::Time,
            Open::Help => LayerId::Help,
            Open::Settings => LayerId::Settings,
            Open::Fields => LayerId::Fields,
            Open::Bookmarks => LayerId::Bookmarks,
            Open::View => LayerId::View,
            Open::Source => LayerId::Source,
            Open::Folding => LayerId::Folding,
            Open::Recipes { .. } => LayerId::Recipes,
            Open::RecipeHistory { .. } => LayerId::RecipeHistory,
            Open::Search | Open::Advanced => LayerId::Filter,
            Open::Grouping => LayerId::Grouping,
            Open::Enrichment => LayerId::Enrichment,
            Open::EnrichmentStep { .. } => LayerId::EnrichmentStep,
            Open::ExternalCommand { .. } => LayerId::ExternalCommand,
            Open::Ask(_) => LayerId::Ask,
            Open::Investigation => LayerId::Investigation,
        }
    }

    /// Whether the layer edits the active view and therefore has nothing to
    /// show without one. `App::handle` used to carry this precondition inside
    /// each `Open*` arm (`Action::OpenViewDialog` opened nothing when the view
    /// list was empty); expressing it as data on `Open` keeps it out of the
    /// router, which stays routing-only (§7.7).
    pub fn needs_active_view(&self) -> bool {
        match self {
            // `Action::OpenSearch`/`OpenAdvanced` carried this guard too: an
            // editor with no view has no draft to edit and no query to submit.
            // The three enrichment layers all edit the active view's pipeline.
            Open::Bookmarks
            | Open::View
            | Open::Folding
            | Open::Search
            | Open::Advanced
            | Open::Grouping
            | Open::Enrichment
            | Open::EnrichmentStep { .. }
            | Open::ExternalCommand { .. } => true,
            // Fields reads the selected row through the provider and opens on
            // an empty view; Time seeds from the active view but opened without
            // one before its conversion; Storage and Help never read views; and
            // Settings declines its own open for a different reason (§6.5), on
            // the shell's side.
            // Recipes opens on an empty workspace too: Browse lists what is
            // saved, and Save/Update check for a view when they submit.
            Open::Storage
            | Open::Time
            | Open::Help
            | Open::Settings
            | Open::Fields
            | Open::Source
            | Open::Recipes { .. }
            | Open::RecipeHistory { .. } => false,
            // Ask freezes the active view's id and definition revision at open
            // and fences every proposal against them, so it has nothing to
            // show without one. Investigation freezes the same pair into the
            // snapshot every session is bound to.
            Open::Ask(_) | Open::Investigation => true,
        }
    }
}

/// Terminal input before the shell has decided what it means. `terminal.rs`
/// produces this instead of an `Action` while a converted layer is on top
/// (§6.4); it is migration-only in the sense that `Action::Raw` is, not the
/// type itself.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RawEvent {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Paste(String),
    Resize,
}

/// Whether a `KeyCode::Char` is text the user typed rather than a chord.
///
/// A terminal encodes Alt-<key> as ESC followed by the key's byte, so a
/// dismissal whose Esc reaches the app in the same read as the next key
/// arrives as one Alt chord. A field whose catch-all `KeyCode::Char` arm
/// inserts regardless of modifiers then swallows the Esc *and* types the key
/// the user pressed as a shortcut — the Search editor typing `t` instead of
/// closing so Time could open. A chord a layer does not bind is not text.
///
/// Shift is not a chord: it is how the character is capitalized, and it
/// arrives alongside the already-capitalized `char`.
#[must_use]
pub fn is_typed_char(key: &KeyEvent) -> bool {
    key.modifiers.difference(KeyModifiers::SHIFT).is_empty()
}

/// Something changed in the shared state a layer may be showing (§4.2).
/// Nothing emits these yet; they arrive with `Views` in the Time conversion.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ViewEvent {
    QueryAccepted {
        view_id: String,
        purpose: QueryPurpose,
        revision: u64,
    },
    QueryRejected {
        view_id: String,
        purpose: QueryPurpose,
        message: String,
    },
    TimeApplied {
        view_id: String,
    },
    RecipeApplied {
        view_id: String,
    },
    SourcesChanged {
        view_id: String,
    },
    SelectionChanged {
        view_id: String,
    },
}

/// Everything a component can receive. Raw keys, not `Action`s: the component
/// owns its keymap.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Event<H> {
    Key(KeyEvent),
    Paste(String),
    Mouse {
        kind: MouseEventKind,
        point: (u16, u16),
        hit: Option<H>,
    },
    /// Escape or `q` reached this layer. The component closes its innermost
    /// popup and returns `Consumed`, or returns `Close`.
    Dismiss,
    /// A palette command the component declared in `commands()`.
    Command(CommandId),
    /// Delivered to every open layer, top first.
    View(ViewEvent),
    /// The terminal was resized; geometry from the last render is stale.
    Resize,
}

/// What the shell does after `handle`. Exactly one per event.
#[must_use]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Outcome {
    /// Not for me. The shell applies its global bindings or drops the event.
    Ignored,
    /// Handled; redraw.
    Consumed,
    /// Pop this layer.
    Close,
    /// Pop this layer and push another in the same tick.
    Replace(Open),
    /// Push a child layer on top of this one.
    OpenChild(Open),
    /// Migration-only, and the mirror of `Action::Raw` (§6.4): pop this layer
    /// and hand a legacy dialog the `Action` that opens it. Time's
    /// `🧠 Recognize timestamp` needs it because Ask is converted last (§6.3);
    /// it becomes `Replace(Open::Ask { .. })` then, and the variant goes.
    Legacy(crate::app::Action),
    /// Migration-only: run a shell `Action` and *stay* on the stack. Used where
    /// the work belongs to a subsystem that is not converted yet (the fork
    /// queue, the correlation queue) or to a dialog converted later that
    /// returns here — Raw context, opened from Fields, comes back to
    /// `Focus::Layer`. Becomes `OpenChild`/a `ctx` call as each lands.
    Defer(crate::app::Action),
}

/// Returned by `render`: what the shell needs for containment, text selection
/// and the terminal caret. Nothing else leaves the component.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Surface {
    /// Border included; modal containment.
    pub popup: Rect,
    /// `hit_regions.selection_modal`.
    pub interior: Rect,
    /// Where the layer put the terminal caret this frame, if it drew one.
    pub caret: Option<(u16, u16)>,
    /// Whether wheel events over `popup` are wanted.
    pub scrollable: bool,
    /// Whether a text field currently has focus. §1 folds this into
    /// `caret.is_none()`, but the two differ: a focused field scrolled out of
    /// the body draws no caret and still takes `q` as a character, so the
    /// dismissal rule reads this and the terminal reads `caret`.
    pub text_focus: bool,
}

/// The agent provider, mode and thinking level `lvu-app` resolved from
/// settings. Pure shell configuration, like `appearance`: `App::configure_ai`
/// and a settings save are its only writers, and Source, Ask and Investigation
/// all read it to stamp an agent request (§6.5).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AgentDefaults {
    pub provider: String,
    pub mode: String,
    pub thinking: String,
}

/// Pure time input (§2.2). `now_unix_nanos` is the shell's sampled clock, not
/// a fresh syscall, so a frame is internally consistent.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Clock {
    pub now_unix_nanos: i64,
}

/// A palette entry a component declares (§4.3). The palette no longer peeks at
/// dialog internals to decide availability; the component answers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommandSpec {
    pub id: CommandId,
    pub name: &'static str,
    pub description: &'static str,
    pub category: &'static str,
    pub aliases: &'static [&'static str],
    pub shortcut: Option<&'static str>,
}

/// A `CommandSpec` plus the component's current availability verdict. §4.3
/// describes `commands()` as returning entries whose availability varies with
/// component state, which the spec's `-> &'static [CommandSpec]` signature
/// cannot express; this is that signature corrected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommandEntry {
    pub spec: CommandSpec,
    pub unavailable_reason: Option<&'static str>,
}

/// Everything a component may touch while handling input. Built by the shell
/// from disjoint fields of `App` (§2.5); never from `&mut App`.
pub struct Ctx<'a> {
    pub views: &'a mut Views,
    /// §2.2's single exception, and only Settings may write it.
    pub appearance: &'a mut Appearance,
    /// Read-only agent configuration (§6.5).
    pub agent: &'a AgentDefaults,
    /// The read half of the `Sources` aggregate §4.2 plans. Membership editing
    /// needs the open sources by name and order; admitting, stopping and
    /// restarting them stays on the shell until step 11 moves the whole
    /// aggregate down, so what components see today is a slice.
    pub sources: &'a [SourceItem],
    pub provider: &'a dyn RowProvider,
    /// Whether the shell's cross-source correlation lookup is in flight. The
    /// second documented exception to §7.2, and the reason it is not component
    /// state: the queue is the shell's, it outlives the Fields layer that
    /// starts it, and a view switch cancels it from outside. Fields only reads
    /// it, to freeze itself; it goes when Correlation converts (§6.3).
    pub correlating: bool,
    pub cursors: &'a mut CursorBank,
    pub clock: Clock,
    pub size: (u16, u16),
    pub ascii: bool,
    notices: &'a mut Option<String>,
}

impl<'a> Ctx<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        views: &'a mut Views,
        sources: &'a [SourceItem],
        appearance: &'a mut Appearance,
        agent: &'a AgentDefaults,
        provider: &'a dyn RowProvider,
        cursors: &'a mut CursorBank,
        notices: &'a mut Option<String>,
        clock: Clock,
        size: (u16, u16),
        correlating: bool,
    ) -> Self {
        let ascii = appearance.ascii;
        Self {
            views,
            sources,
            appearance,
            agent,
            provider,
            correlating,
            cursors,
            clock,
            size,
            ascii,
            notices,
        }
    }

    /// The one-line status-bar message: a shell surface with one writer.
    pub fn notice(&mut self, text: impl Into<String>) {
        *self.notices = Some(text.into());
    }
}

/// The empty provider the shell uses to build a `Ctx` on completion paths that
/// hold no provider: `lvu-app` answers a `ViewMutationRequest` from its request
/// loop, not from the render/input loop, and the resulting `ViewEvent` still has
/// to reach every open layer. §4.2 says a layer decides what a view event means
/// from `Views`, never from rows, so serving no rows here is the contract rather
/// than a shortcut.
pub(crate) struct NoRows;

pub(crate) static NO_ROWS: NoRows = NoRows;

impl RowProvider for NoRows {
    fn page(&self, _view_id: &str, _request: crate::provider::ViewportRequest) -> RowPage {
        RowPage {
            total: 0,
            rows: Vec::new(),
        }
    }

    fn row_by_id(&self, _view_id: &str, _id: &RowId) -> Option<DisplayRow> {
        None
    }

    fn index_of_id(&self, _view_id: &str, _id: &RowId) -> Option<usize> {
        None
    }

    fn revision(&self, _view_id: &str) -> u64 {
        0
    }
}

/// The read-only subset available during `render`.
pub struct RenderCtx<'a> {
    pub views: &'a Views,
    /// Read-only carets, for the layers whose drafts live in `ViewState` and
    /// whose caret therefore lives in the bank (§2.2). `Ctx` hands out `&mut`;
    /// a render only ever `peek`s.
    pub cursors: &'a CursorBank,
    pub sources: &'a [SourceItem],
    pub provider: &'a dyn RowProvider,
    /// See `Ctx::correlating`.
    pub correlating: bool,
    /// Whole-view field statistics, when a pass has answered. Shell state for
    /// the same reason `correlating` is: the pass outlives the Fields layer
    /// that prompted it, and a view switch cancels it from outside.
    pub whole_view_stats: Option<&'a crate::app::WholeViewStats>,
    /// A whole-view pass is out and has not answered yet.
    pub field_stats_pending: bool,
    /// Whether this layer is the top of the stack. §10 of `dialog-system.md`
    /// says a parent under a child keeps its title and frame but drops to
    /// `border` colour; only the parent knows how much of its own body that
    /// leaves drawable, so the shell tells it which of the two it is rather
    /// than trying to repaint the frame afterwards (§6.5).
    pub active: bool,
    pub theme: Theme,
    pub ascii: bool,
    /// The fixed UTC offset the log viewport draws times in. Read-only here:
    /// it is the reader's setting, and a layer that reports it must report the
    /// same one the rows behind it are using.
    pub display_zone: &'a str,
    pub size: (u16, u16),
    pub clock: Clock,
}

pub trait Component {
    /// Clickable things this component draws. `Copy` so the shell can hand it
    /// back inside a mouse event without borrowing the component.
    type Hit: Copy + Eq;

    /// Parameters needed to open (or re-open) this layer.
    type Open;

    /// Called by the shell when the layer is pushed.
    fn open(&mut self, params: Self::Open, ctx: &mut Ctx<'_>);

    /// Input for this layer. Returns what the shell must do next.
    fn handle(&mut self, event: Event<Self::Hit>, ctx: &mut Ctx<'_>) -> Outcome;

    /// Draws the layer and records its own geometry. `&mut self` because
    /// geometry is state: the same rects serve rendering, mouse hit-testing,
    /// scrolling and text selection (AGENTS.md).
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx<'_>) -> Surface;

    /// The `Surface` the last `render` produced. The shell uses it for modal
    /// containment (§5.2) and selection bounds (§3) between frames.
    fn surface(&self) -> Surface;

    /// Whether a text field has focus *right now*. The shell asks this to
    /// decide whether bare `q` dismisses or types (§1). It defaults to what the
    /// last render published, which is right whenever a redraw separates the
    /// focus change from the next key; a layer whose focus can move without a
    /// redraw in between — a click that opens an input — overrides it with its
    /// live state.
    fn text_focus(&self) -> bool {
        self.surface().text_focus
    }

    /// §8.10: this layer's action row as it stands right now — the same
    /// labels, in the same order, that `render` hands to `ui::render_actions`,
    /// `&` markers included. The shell reads it to resolve a mnemonic before
    /// the component sees the key, so the underlined letter presses the button
    /// in every dialog through one mechanism instead of thirteen keymaps.
    ///
    /// A layer with no action row (Help) leaves this empty and no key is ever
    /// diverted from it. A layer whose row varies with state — Fields' `Pin` /
    /// `Unpin`, Storage's `Preview cleanup` / `Confirm cleanup` — must compute
    /// it here and in `render` from one function, exactly as §8.9 requires of
    /// the default action, so the drawn underline and the live key cannot
    /// disagree.
    fn action_labels(&self, _ctx: &Ctx<'_>) -> Vec<&'static str> {
        Vec::new()
    }

    /// Press the button at `index` of `action_labels`, as a click on it would.
    /// The focus ring does not move: an accelerator fires its verb where the
    /// user is, and only the mouse both focuses and fires.
    fn press_action(&mut self, _index: usize, _ctx: &mut Ctx<'_>) -> Outcome {
        Outcome::Ignored
    }

    /// Resolve a screen point against the rects recorded by the last `render`.
    fn hit(&self, point: (u16, u16)) -> Option<Self::Hit>;

    /// Palette entries this layer contributes (§4.3). Availability may depend
    /// on the shared view state, which is why `Views` is passed: the palette is
    /// assembled where no provider or theme exists, so a full `RenderCtx` — what
    /// §1 sketches — cannot be built there.
    fn commands(&self, _views: &Views) -> Vec<CommandEntry> {
        Vec::new()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OutboxFull;

/// A component's bounded request queue plus its completion fence (§2.4).
/// `lvu-app` drains it and posts completions back through the component's own
/// `complete`, so no request queue lives on `App`.
#[derive(Clone, Debug)]
pub struct Outbox<Req> {
    queue: VecDeque<Req>,
    next_generation: u64,
    cap: usize,
}

impl<Req> Outbox<Req> {
    pub fn new(cap: usize) -> Self {
        Self {
            queue: VecDeque::new(),
            next_generation: 1,
            cap,
        }
    }

    pub fn push(&mut self, req: Req) -> Result<(), OutboxFull> {
        if self.queue.len() >= self.cap {
            return Err(OutboxFull);
        }
        self.queue.push_back(req);
        Ok(())
    }

    /// Allocates the next fence value. Completions carrying an older one are
    /// stale and must be ignored by the component.
    pub fn next_generation(&mut self) -> u64 {
        let generation = self.next_generation;
        self.next_generation = generation.saturating_add(1);
        generation
    }

    pub fn take(&mut self) -> Vec<Req> {
        self.queue.drain(..).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// Requests not yet drained. A component whose own refusal threshold is
    /// lower than `cap` — Settings words one at two pending saves — reads it.
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    /// The queued requests, for a component that refuses per kind rather than
    /// per queue (Source, §8).
    pub fn iter(&self) -> impl Iterator<Item = &Req> {
        self.queue.iter()
    }

    /// Drains the requests one consumer recognises and leaves the rest queued,
    /// preserving order within a kind. §8 leaves it open whether `lvu-app`
    /// merges Source's four loops or keeps four drains of its one queue; it
    /// keeps four, and this is how one queue serves them.
    pub fn take_where<T>(&mut self, pick: impl Fn(Req) -> Result<T, Req>) -> Vec<T> {
        let mut taken = Vec::new();
        let mut kept = VecDeque::with_capacity(self.queue.len());
        for request in std::mem::take(&mut self.queue) {
            match pick(request) {
                Ok(value) => taken.push(value),
                Err(request) => kept.push_back(request),
            }
        }
        self.queue = kept;
        taken
    }
}
