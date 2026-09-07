//! The component contract (`docs/component-model.md` §1, §2, §5).
//!
//! A component owns its state, its keymap, its geometry and its background
//! requests. It receives raw input and a `Ctx` built from disjoint fields of
//! `App`; it never sees `App` itself, so the boundary is compiler-enforced.
//!
//! Migration status: `Ctx` carries `views` (extracted from `App` with the Time
//! conversion, §6.2) and `appearance` (added with Settings, §6.3 step 10).

use std::collections::VecDeque;

use crossterm::event::{KeyEvent, MouseEvent, MouseEventKind};
use ratatui::{Frame, layout::Rect};

use crate::app::{QueryPurpose, Views};
use crate::command_palette::CommandId;
use crate::provider::RowProvider;
use crate::text_edit::CursorBank;
use crate::theme::{Theme, ThemeId};

/// The shell's appearance state, and the one documented exception to "no
/// component-specific field in `Ctx`" (§2.2): Settings previews a draft theme,
/// delight, reduced-motion or ASCII choice by writing it live and rolls it back
/// from its own saved baseline on dismissal. Every other reader takes it
/// through `RenderCtx.theme` / `RenderCtx.ascii`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Appearance {
    pub theme_id: ThemeId,
    pub delight_enabled: bool,
    pub reduced_motion: bool,
    pub ascii: bool,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            theme_id: ThemeId::Terminal,
            delight_enabled: std::env::var_os("LVU_NO_DELIGHT").is_none(),
            reduced_motion: std::env::var_os("LVU_REDUCED_MOTION").is_some(),
            ascii: std::env::var_os("LVU_ASCII").is_some(),
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
}

/// Constructors for every layer the shell knows how to host (§1). Grows by
/// exactly one variant per conversion.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Open {
    Storage,
    Time,
    Help,
    Settings,
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
    pub provider: &'a dyn RowProvider,
    pub cursors: &'a mut CursorBank,
    pub clock: Clock,
    pub size: (u16, u16),
    pub ascii: bool,
    notices: &'a mut Option<String>,
}

impl<'a> Ctx<'a> {
    pub fn new(
        views: &'a mut Views,
        appearance: &'a mut Appearance,
        provider: &'a dyn RowProvider,
        cursors: &'a mut CursorBank,
        notices: &'a mut Option<String>,
        clock: Clock,
        size: (u16, u16),
    ) -> Self {
        let ascii = appearance.ascii;
        Self {
            views,
            appearance,
            provider,
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

/// The read-only subset available during `render`.
pub struct RenderCtx<'a> {
    pub views: &'a Views,
    pub provider: &'a dyn RowProvider,
    pub theme: Theme,
    pub ascii: bool,
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
}
