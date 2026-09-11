//! Shared dialog geometry: size classes, the single region layout function, the
//! backdrop scrim and the pane primitive.
//!
//! Implements `docs/dialog-system.md` §3 (anatomy), §4.1 (spacing scale),
//! §5 (sizing) and §6.2 (scrim). Dialogs supply content row counts and get back
//! rects; no dialog computes geometry of its own, so rendering, scrolling,
//! mouse hitboxes and text selection cannot drift apart.

use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Flex, Layout, Margin, Rect},
    style::{Color, Style},
    widgets::{Block, Padding},
};
use unicode_width::UnicodeWidthStr;

use crate::dialog_controls::{self, ActionGeometry};
use crate::theme::Theme;

/// Size classes from §5.1. `A` (anchored popups) is not a dialog: it is placed
/// against a field rather than the frame, so it has its own constructor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DialogClass {
    /// One field with a live effect. Top-biased so it does not cover its result.
    S,
    /// A short form or short list.
    M,
    /// A multi-part form, a long list or a transcript.
    L,
    /// Read-mostly content that benefits from every column.
    /// The command palette: transient, top-anchored, list-driven.
    P,
}

/// §5.1. Below this the classes collapse to one full-frame width and the taller
/// height caps, because a centred dialog with margins wastes cells it needs.
pub fn is_compact(area: Rect) -> bool {
    area.width < 64 || area.height < 20
}

/// A dialog never renders narrower than this; below `ui::layout`'s 20x6 floor
/// the `terminal too small` fallback takes over instead.
const MIN_DIALOG_WIDTH: u16 = 20;

/// §4.1: pads and gaps exist only when the interior can afford them.
const PAD_THRESHOLD: u16 = 14;
/// §5.4 step 4: below this the body scrolls rather than shrinking further.
pub const MIN_BODY_ROWS: u16 = 3;

fn scaled(value: u16, percent: u16) -> u16 {
    // Round to nearest so the class table in §5.3 is reproduced exactly.
    (u32::from(value) * u32::from(percent) + 50).div_euclid(100) as u16
}

impl DialogClass {
    /// §5.1 width rule for this class on `area`.
    pub fn width(self, area: Rect) -> u16 {
        // Compact terminals collapse every class to the full frame, because
        // margins cost cells they need. (Class XL, the full frame everywhere,
        // was retired with the Raw context dialog; see raw-context-as-jump.md.)
        let full = area.width.saturating_sub(2);
        let width = if is_compact(area) {
            full
        } else {
            match self {
                Self::S => scaled(area.width, 60).clamp(48, 72),
                Self::M => scaled(area.width, 72).clamp(60, 96),
                Self::L => scaled(area.width, 86).clamp(72, 132),
                Self::P => scaled(area.width, 64).clamp(50, 92),
            }
        };
        width.min(area.width).max(MIN_DIALOG_WIDTH.min(area.width))
    }

    /// §5.1 maximum height. Actual height follows content (§5.2).
    pub fn max_height(self, area: Rect) -> u16 {
        let height = if is_compact(area) {
            match self {
                Self::S | Self::M => area.height.saturating_sub(2),
                Self::L | Self::P => area.height,
            }
        } else {
            match self {
                Self::S => 12,
                Self::M => area.height.saturating_sub(4),
                Self::L => area.height.saturating_sub(2),
                Self::P => area.height.saturating_sub(3),
            }
        };
        height.min(area.height).max(3.min(area.height))
    }

    fn top_biased(self) -> bool {
        matches!(self, Self::S | Self::P)
    }
}

/// Natural row counts for a dialog's regions, measured at the class content
/// width. `body` is the body's *natural* height (§5.2); the class cap and the
/// degradation order decide what it actually gets.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DialogContent {
    /// 0 or 1: a segmented mode control, or one summary line.
    pub header: u16,
    /// Every form row, list row and pane row the dialog would like to show.
    pub body: u16,
    /// 0 when the dialog has no state, otherwise the wrapped message rows.
    pub message: u16,
    /// 0..=2 wrapped help rows.
    pub help: u16,
    /// Rows the action row needs, 0 when there are no buttons.
    pub actions: u16,
}

impl DialogContent {
    /// Chrome rows for a given pad/gap and help allowance (§3 arithmetic).
    fn fixed(&self, pad: u16, help: u16) -> u16 {
        let gap = pad;
        let mut total = pad.saturating_mul(2);
        if self.header > 0 {
            total = total.saturating_add(self.header).saturating_add(gap);
        }
        let tail = self.message.saturating_add(help);
        if tail > 0 {
            total = total.saturating_add(tail).saturating_add(gap);
        }
        if self.actions > 0 {
            total = total.saturating_add(self.actions).saturating_add(gap);
        }
        total
    }

    /// Interior rows this content wants, with every region it asked for. Only
    /// `pad` depends on the result (§4.1), so settle that by iterating from the
    /// roomy assumption; it converges in at most two steps.
    ///
    /// Help is *not* dropped here. Height is content-driven, so a naturally
    /// short dialog would otherwise drop its help for being short — which is
    /// circular, and contradicts the §12.1 mockup of an eight-row Search that
    /// keeps its examples row. Dropping is height *pressure*, applied in
    /// `regions` when the content genuinely does not fit.
    pub fn interior_rows(&self) -> u16 {
        let mut pad = 1;
        let mut rows = self.fixed(pad, self.help).saturating_add(self.body);
        for _ in 0..3 {
            let next_pad = u16::from(rows >= PAD_THRESHOLD);
            let next = self.fixed(next_pad, self.help).saturating_add(self.body);
            if next_pad == pad && next == rows {
                break;
            }
            pad = next_pad;
            rows = next;
        }
        rows.max(1)
    }
}

/// Every rect a dialog needs, all derived from one popup (§3). Empty regions
/// have zero height and are never drawn into.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DialogRegions {
    pub popup: Rect,
    /// Inside the border. This is also the selection and hit-test surface.
    pub interior: Rect,
    /// Inside the `side` padding: where every row starts (§4.1).
    pub content: Rect,
    pub header: Rect,
    pub body: Rect,
    pub message: Rect,
    pub help: Rect,
    pub actions: Rect,
    /// Natural body rows minus the rows the body actually got: the body scrolls
    /// by this much (§9), and it is 0 when everything fits.
    pub body_overflow: u16,
}

/// What a given interior height actually yields after §5.4 degradation.
///
/// Sizing and layout must not disagree about this: if `dialog_rect` assumed the
/// pads that `regions` then sheds, the freed rows have nowhere to go but the
/// body, and a dialog that shed padding to fit ends up padded out with blank
/// rows instead of getting shorter. Both call this.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Fit {
    pad: u16,
    help: u16,
    message: u16,
    actions: u16,
    /// Rows the body would use at this height. Capped at what it asked for,
    /// because rows it has no content for are dead space — but this cap is for
    /// *sizing* only. `regions` still hands the body every leftover row, so a
    /// dialog that renders more than it declared is shortened, never starved.
    body: u16,
    /// Every chrome row at this fit.
    fixed: u16,
}

impl Fit {
    /// Interior rows this fit occupies.
    fn used(self) -> u16 {
        self.fixed.saturating_add(self.body)
    }
}

fn fit(interior_height: u16, content: &DialogContent) -> Fit {
    let mut pad = u16::from(interior_height >= PAD_THRESHOLD);
    let mut help = content.help;
    let mut message = content.message;
    let mut actions = content.actions;
    let plan = |pad: u16, help: u16, message: u16, actions: u16| {
        DialogContent {
            header: content.header,
            body: content.body,
            message,
            help,
            actions,
        }
        .fixed(pad, help)
    };
    let mut fixed = plan(pad, help, message, actions);
    // §5.4 orders height pressure: help goes first, then pads and gaps, and
    // only then does the body scroll. The pressure test is whether the body
    // gets the rows it asked for — not whether it clears a fixed floor, which
    // let a content-heavy body be squeezed to nothing while help kept its rows.
    let squeezed = |fixed: u16| interior_height < fixed.saturating_add(content.body);
    if squeezed(fixed) && help > 0 {
        help = 0;
        fixed = plan(pad, help, message, actions);
    }
    if squeezed(fixed) && pad > 0 {
        pad = 0;
        fixed = plan(pad, help, message, actions);
    }
    // The body is the one region that must never reach zero: the selection
    // lives in it, and a modal whose selection is off-surface is a bug. Once
    // pads and help are gone, take rows back from a wrapped action row, then
    // from the message, rather than starving it.
    while interior_height.saturating_sub(fixed) == 0 && actions > 1 {
        actions -= 1;
        fixed = plan(pad, help, message, actions);
    }
    while interior_height.saturating_sub(fixed) == 0 && message > 0 {
        message -= 1;
        fixed = plan(pad, help, message, actions);
    }
    if interior_height.saturating_sub(fixed) == 0 && actions > 0 {
        actions = 0;
        fixed = plan(pad, help, message, actions);
    }
    Fit {
        pad,
        help,
        message,
        actions,
        body: interior_height.saturating_sub(fixed).min(content.body),
        fixed,
    }
}

/// §5.2. `content` is measured by the caller at `class.width(area) - 4`.
pub fn dialog_rect(area: Rect, class: DialogClass, content: &DialogContent) -> Rect {
    let width = class.width(area);
    let max_height = class.max_height(area).min(area.height);
    // Start from everything the class allows and give back what the content
    // does not use. Shrinking can drop the interior below `PAD_THRESHOLD`,
    // which frees the pads and lets it shrink again, so this settles rather
    // than assuming one pass; it converges in at most two steps.
    let mut interior = max_height.saturating_sub(2);
    for _ in 0..3 {
        let used = fit(interior, content).used().max(1);
        if used >= interior {
            break;
        }
        interior = used;
    }
    let height = interior
        .saturating_add(2)
        .clamp(3.min(max_height), max_height)
        .min(area.height);
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = if class.top_biased() {
        let offset = (area.height / 6).max(1);
        area.y + offset.min(area.height.saturating_sub(height))
    } else {
        area.y + area.height.saturating_sub(height) / 2
    };
    Rect::new(x, y, width, height)
}

/// The rect a class occupies for a full-height dialog, for tests and callers
/// that need the class geometry without measuring content.
pub fn dialog_rect_for_class(area: Rect, class: DialogClass) -> Rect {
    dialog_rect(
        area,
        class,
        &DialogContent {
            body: u16::MAX,
            ..DialogContent::default()
        },
    )
}

/// Interior rows `content` actually occupies at `interior_height`, after §5.4
/// degradation. Exposed so a test can state the sizing invariant directly: a
/// dialog's interior is the rows it uses, never more.
pub fn fitted_rows(interior_height: u16, content: &DialogContent) -> u16 {
    fit(interior_height, content).used()
}

/// §5.2.1: a live region never renders fewer rows than this, however tight the
/// frame is, because a one-row list is not a list.
pub const MIN_LIVE_ROWS: u16 = 2;

/// §5.2.1. Rows to reserve for a region whose content changes while the user
/// types — a completion list, a discovery candidate list, a live preview.
///
/// The count comes from the frame, the class and the dialog's *stable* content
/// alone; the number of items in the region is deliberately not an input. A
/// list that grows past its reservation scrolls behind the pane's count and
/// scrollbar (§9); one that shrinks leaves blank rows. Either way the popup
/// rect is identical from one keystroke to the next, which is the whole point:
/// content-driven height (§5.2) is only legible when the content is stable.
///
/// `stable` is the same `DialogContent` the dialog will hand to `dialog_rect`
/// but with the live region's rows left out, and `desired` is the region's
/// natural height (heading plus the list rows it would like).
pub fn live_rows(area: Rect, class: DialogClass, stable: &DialogContent, desired: u16) -> u16 {
    let interior = class.max_height(area).saturating_sub(2);
    let spare = interior.saturating_sub(stable.interior_rows());
    desired.min(spare).max(MIN_LIVE_ROWS.min(spare))
}

/// The content width a dialog measures its regions against before it knows its
/// own height: border and `side` padding on both edges.
pub fn content_width(area: Rect, class: DialogClass) -> u16 {
    class.width(area).saturating_sub(4)
}

/// §3. The only layout function dialogs use.
pub fn regions(popup: Rect, content: &DialogContent) -> DialogRegions {
    let interior = popup.inner(Margin::new(1, 1));
    let inner = if interior.width > 2 {
        interior.inner(Margin::new(1, 0))
    } else {
        interior
    };
    let empty = Rect::new(inner.x, inner.y, inner.width, 0);
    if interior.height == 0 || inner.width == 0 {
        return DialogRegions {
            popup,
            interior,
            content: inner,
            header: empty,
            body: empty,
            message: empty,
            help: empty,
            actions: empty,
            body_overflow: content.body,
        };
    }

    // §5.4 degradation order, shared with `dialog_rect` so the height a dialog
    // was given and the rows it lays out inside it cannot disagree.
    let Fit {
        pad,
        help,
        message,
        actions,
        fixed,
        ..
    } = fit(interior.height, content);
    let gap = pad;
    // The body takes what is left. `dialog_rect` has already sized the popup so
    // that "what is left" is what the body asked for; when it could not — the
    // class cap, or a dialog that draws more rows than it declared — the body
    // is the right place for the difference.
    let body_rows = interior.height.saturating_sub(fixed);

    let mut cursor = inner.y.saturating_add(pad);
    let mut take = |rows: u16| {
        let rect = Rect::new(inner.x, cursor.min(inner.bottom()), inner.width, rows);
        cursor = cursor.saturating_add(rows);
        rect
    };

    let header = take(content.header);
    if content.header > 0 {
        take(gap);
    }
    let body = take(body_rows);
    let tail = message.saturating_add(help);
    if tail > 0 {
        take(gap);
    }
    let message_rect = take(message);
    let help_rect = take(help);
    if actions > 0 {
        take(gap);
    }
    let actions_rect = take(actions);

    DialogRegions {
        popup,
        interior,
        content: inner,
        header,
        body,
        message: message_rect,
        help: help_rect,
        actions: actions_rect,
        body_overflow: content.body.saturating_sub(body_rows),
    }
}

/// How far a block cell's two pixels are pushed toward the backdrop by the
/// scrim.
///
/// Deliberately gentle. The scrim exists to make the backdrop inactive, and the
/// indicator art is *already* quieter than the text around it: its lit pixels
/// measure 3.25 against the dark base where scrimmed `muted` text measures
/// 6.18. Pushing the art as far as text goes would put it below the threshold
/// of being a shape at all. A third of the way recedes it — 3.25 to 1.99 —
/// while both halves of every cell keep moving together, which is what
/// preserves the shape.
const SCRIM_DIM: f64 = 0.35;

/// Whether this cell is a picture rather than text.
///
/// A half-block or shade glyph draws *two pixels* — its foreground is the upper
/// (or filled) one and its background the lower — so its shape is the contrast
/// between fg and bg, not the glyph. Flattening the foreground to `muted` on
/// such a cell does not dim it, it erases half of it: the indicator heart under
/// an open dialog became a grey bar over a red bar.
fn is_picture(symbol: &str) -> bool {
    matches!(
        symbol,
        "\u{2580}" // ▀ upper half
            | "\u{2584}" // ▄ lower half
            | "\u{2588}" // █ full
            | "\u{258c}" // ▌ left half
            | "\u{2590}" // ▐ right half
            | "\u{2591}" // ░
            | "\u{2592}" // ▒
            | "\u{2593}" // ▓
    )
}

/// `color` moved `amount` of the way toward `toward`, or `None` when either is
/// a colour whose displayed value lvu does not know (the terminal default, or a
/// named ANSI colour the user may have remapped).
fn dimmed(color: Color, toward: Color, amount: f64) -> Option<Color> {
    let (red, green, blue) = crate::theme::resolved_rgb(color)?;
    let (to_red, to_green, to_blue) = crate::theme::resolved_rgb(toward)?;
    let mix = |from: u8, to: u8| {
        (f64::from(from) + (f64::from(to) - f64::from(from)) * amount).round() as u8
    };
    Some(Color::Rgb(
        mix(red, to_red),
        mix(green, to_green),
        mix(blue, to_blue),
    ))
}

/// §6.2. Restyle the workspace behind an open dialog: muted foreground, no
/// modifiers, background untouched. A style pass over the buffer, not a widget:
/// it moves no cell and changes no hit region.
///
/// §5.2's shape rule is the one exception: a block-drawing cell is a picture,
/// and a picture is dimmed by moving *both* of its pixels toward the backdrop
/// rather than by repainting one of them muted. The two halves keep their
/// relative contrast, so the art still reads as its own shape, only quieter.
/// Where lvu cannot know what a colour displays as — `Color::Reset`, or a named
/// ANSI colour on a terminal whose palette the user may have remapped — there
/// is nothing safe to blend, so the cell is left as it is: an undimmed picture
/// is still the picture, while a half-erased one is nothing at all.
pub fn scrim(buffer: &mut Buffer, area: Rect, theme: Theme) {
    let area = area.intersection(buffer.area);
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            let cell = &mut buffer[(x, y)];
            let background = cell.style().bg.unwrap_or(theme.base_bg);
            if is_picture(cell.symbol()) {
                let backdrop = theme.base_bg;
                let foreground = cell.style().fg.unwrap_or(theme.base_fg);
                if let (Some(fg), Some(bg)) = (
                    dimmed(foreground, backdrop, SCRIM_DIM),
                    dimmed(background, backdrop, SCRIM_DIM),
                ) {
                    cell.set_style(Style::reset().fg(fg).bg(bg).underline_color(Color::Reset));
                } else {
                    cell.set_style(
                        Style::reset()
                            .fg(foreground)
                            .bg(background)
                            .underline_color(Color::Reset),
                    );
                }
                continue;
            }
            cell.set_style(
                Style::reset()
                    .fg(theme.muted)
                    .bg(background)
                    .underline_color(Color::Reset),
            );
        }
    }
}

/// §8.7. A pane is a bold heading with an optional right-aligned count, an
/// indented viewport, and a scrollbar only when the content overflows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PaneRects {
    pub heading: Rect,
    /// Where the right-aligned count is drawn, empty when there is none.
    pub count: Rect,
    pub viewport: Rect,
    /// Present only when `lines` exceeds the viewport (§9).
    pub scrollbar: Option<Rect>,
}

/// §4.1 `indent`.
pub const PANE_INDENT: u16 = 2;

pub fn pane(area: Rect, count_width: u16, lines: usize) -> PaneRects {
    // With a single row to spend, the rows are worth more than the heading.
    let heading_rows = if area.height <= 1 { 0 } else { 1 };
    let heading = Rect::new(area.x, area.y, area.width, heading_rows.min(area.height));
    let count = if count_width == 0 || count_width >= area.width {
        Rect::new(area.right(), area.y, 0, 0)
    } else {
        Rect::new(
            area.right().saturating_sub(count_width),
            area.y,
            count_width,
            heading.height,
        )
    };
    let body_height = area.height.saturating_sub(heading.height);
    // The indent goes too when it would cost more than it communicates.
    let indent = if body_height <= 1 && area.width < 24 {
        0
    } else {
        PANE_INDENT.min(area.width)
    };
    let overflows = lines > usize::from(body_height);
    let scrollbar_width = u16::from(overflows && area.width > indent + 1);
    let viewport = Rect::new(
        area.x.saturating_add(indent),
        area.y.saturating_add(heading.height),
        area.width
            .saturating_sub(indent)
            .saturating_sub(scrollbar_width),
        body_height,
    );
    let scrollbar = (scrollbar_width > 0).then(|| {
        Rect::new(
            area.right().saturating_sub(1),
            viewport.y,
            1,
            viewport.height,
        )
    });
    PaneRects {
        heading,
        count,
        viewport,
        scrollbar,
    }
}

/// §5.1 class A: a popup anchored to the field that opened it, below when there
/// is room and above otherwise. Not a dialog — no scrim, no breadcrumb.
pub fn anchored_rect(area: Rect, field: Rect, items: usize, width_hint: u16) -> Rect {
    let width = width_hint
        .max(field.width)
        .max(12)
        .min(area.right().saturating_sub(field.x).max(1));
    let height = (u16::try_from(items.min(8)).unwrap_or(8)).saturating_add(2);
    let height = height.min(area.height);
    let below = field.bottom();
    let y = if below.saturating_add(height) <= area.bottom() {
        below
    } else {
        field.y.saturating_sub(height).max(area.y)
    };
    let x = field.x.min(area.right().saturating_sub(width));
    Rect::new(x, y, width, height)
}

// --- Responsive presentation policy (phase A foundation) ---
//
// Centralized, reusable geometry for every dialog. Outer size depends only on
// the presentation policy plus stable region budgets, never on async item
// counts or transcript length, so one result drives later render, scroll,
// cursor, selection and hitbox consumers. Existing `DialogClass` callers keep
// working through the compatibility adapters below; no component is switched
// to a new kind in this phase.

/// Tiny floor from `ui::layout`: below this the terminal-too-small fallback
/// owns the frame and no ordinary dialog is attempted.
pub const TINY_MIN_WIDTH: u16 = 20;
/// Tiny floor height (see [`TINY_MIN_WIDTH`]).
pub const TINY_MIN_HEIGHT: u16 = 6;
/// Ordinary dialogs leave at least one cell between frame and viewport edge.
pub const FRAME_INSET_CELLS: u16 = 1;

/// Contextual footprint: a prompt edits one live field, an inspector explains
/// a frozen visible row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContextFootprint {
    Prompt,
    Inspector,
}

/// Responsive presentation kind.
///
/// `FullFrame` is reserved for the startup title and the terminal-too-small
/// fallback; no ordinary dialog adopts it (see [`PresentationKind::is_ordinary`]).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PresentationKind {
    Contextual(ContextFootprint),
    SelfContainedForm,
    LongContent,
    Palette,
    FullFrame,
}

impl PresentationKind {
    /// False only for the reserved [`PresentationKind::FullFrame`].
    pub fn is_ordinary(&self) -> bool {
        !matches!(self, Self::FullFrame)
    }

    /// Top-biased at `H/8`: prompts and the palette stay near the work.
    pub fn top_biased(&self) -> bool {
        matches!(
            self,
            Self::Contextual(ContextFootprint::Prompt) | Self::Palette
        )
    }

    /// Reference mapping from the legacy class. For migration planning only;
    /// no caller switches kinds through this in phase A.
    pub fn from_class(class: DialogClass) -> Self {
        match class {
            DialogClass::S => Self::Contextual(ContextFootprint::Prompt),
            DialogClass::M => Self::SelfContainedForm,
            DialogClass::L => Self::LongContent,
            DialogClass::P => Self::Palette,
        }
    }

    /// (percent, min, max) width tokens, border included.
    fn width_policy(&self) -> (u16, u16, u16) {
        match self {
            Self::Contextual(ContextFootprint::Prompt) => (68, 48, 96),
            Self::Contextual(ContextFootprint::Inspector) => (82, 64, 144),
            Self::SelfContainedForm => (78, 64, 120),
            Self::LongContent => (90, 72, 160),
            Self::Palette => (64, 50, 96),
            Self::FullFrame => (100, 0, u16::MAX),
        }
    }

    /// (percent, min, max) height tokens, border included.
    fn height_policy(&self) -> (u16, u16, u16) {
        match self {
            Self::Contextual(ContextFootprint::Prompt) => (36, 11, 16),
            Self::Contextual(ContextFootprint::Inspector) => (62, 14, 36),
            Self::SelfContainedForm => (58, 14, 36),
            Self::LongContent => (86, 18, 64),
            Self::Palette => (60, 12, 30),
            Self::FullFrame => (100, 0, u16::MAX),
        }
    }
}

/// Percentage share via [`Constraint::Percentage`], so the policy is structural:
/// rounded to nearest by Ratatui, then clamped by the caller.
fn percent_share(total: u16, percent: u16) -> u16 {
    if total == 0 {
        return 0;
    }
    let probe = Rect::new(0, 0, total, 1);
    Layout::horizontal([Constraint::Percentage(percent)]).split(probe)[0].width
}

fn percent_height_share(total: u16, percent: u16) -> u16 {
    if total == 0 {
        return 0;
    }
    let probe = Rect::new(0, 0, 1, total);
    Layout::vertical([Constraint::Percentage(percent)]).split(probe)[0].height
}

/// Clamped policy extent for one axis, capped to a one-cell viewport inset.
fn policy_extent(total: u16, percent: u16, min: u16, max: u16) -> u16 {
    let share = percent_share(total, percent);
    let clamped = share.clamp(min.min(max), max);
    let inset_cap = total.saturating_sub(FRAME_INSET_CELLS.saturating_mul(2));
    clamped.min(inset_cap.max(1)).max(1).min(total.max(1))
}

fn policy_height_extent(total: u16, percent: u16, min: u16, max: u16) -> u16 {
    let share = percent_height_share(total, percent);
    let clamped = share.clamp(min.min(max), max);
    let inset_cap = total.saturating_sub(FRAME_INSET_CELLS.saturating_mul(2));
    clamped.min(inset_cap.max(1)).max(1).min(total.max(1))
}

/// True below the 20x6 floor: the caller draws the terminal-too-small surface.
pub fn is_tiny(viewport: Rect) -> bool {
    viewport.width < TINY_MIN_WIDTH || viewport.height < TINY_MIN_HEIGHT
}

/// Policy frame size for `kind` on `viewport`.
///
/// - Tiny viewports are rejected by the caller (see [`is_tiny`]); this returns
///   the full frame there so callers never divide by zero.
/// - Compact (`W < 64 || H < 20`) collapses every ordinary kind to `W-2 x H-2`,
///   or the full frame when that cannot satisfy the 20x6 safety floor.
/// - Roomy viewports use percentage/clamp tokens capped to a one-cell inset.
/// - `FullFrame` always returns the viewport.
pub fn policy_size(viewport: Rect, kind: PresentationKind) -> (u16, u16) {
    if matches!(kind, PresentationKind::FullFrame) {
        return (viewport.width, viewport.height);
    }
    if is_tiny(viewport) {
        return (viewport.width, viewport.height);
    }
    if is_compact(viewport) {
        let width = viewport.width.saturating_sub(2);
        let height = viewport.height.saturating_sub(2);
        if width < TINY_MIN_WIDTH || height < TINY_MIN_HEIGHT {
            return (viewport.width, viewport.height);
        }
        return (width.max(1), height.max(1));
    }
    let (wp, wmin, wmax) = kind.width_policy();
    let (hp, hmin, hmax) = kind.height_policy();
    let width = policy_extent(viewport.width, wp, wmin, wmax)
        .max(TINY_MIN_WIDTH.min(viewport.width))
        .min(viewport.width);
    let height = policy_height_extent(viewport.height, hp, hmin, hmax).min(viewport.height);
    (width.max(1), height.max(1))
}

/// Block padding for the dialog frame.
///
/// Roomy uses one cell of padding inside the border (total side 2, matching
/// §4.1 `side`, with top/bottom pad rows); compact drops the vertical pads.
/// Expressed via [`Block::padding`] so rendering and geometry share it. The
/// reviewed plan names these totals as `Padding::new(2, 2, 1, 1)`; with
/// `Block::bordered` the border supplies one cell, so the padding itself is
/// `Padding::new(1, 1, 1, 1)` roomy and `Padding::new(1, 1, 0, 0)` compact.
pub fn frame_padding(is_compact_viewport: bool) -> Padding {
    if is_compact_viewport {
        Padding::new(1, 1, 0, 0)
    } else {
        Padding::new(1, 1, 1, 1)
    }
}

/// Shared frame block: bordered in the caller's color, padded per viewport.
pub fn frame_block(is_compact_viewport: bool) -> Block<'static> {
    Block::bordered().padding(frame_padding(is_compact_viewport))
}

/// Frozen opening-row anchor for contextual inspectors (pure seam, unwired).
///
/// Captured when the first layer opens and retained across async frames and
/// `Replace`; it never chases live row movement. `log` is the log viewport
/// the dialog is centered over horizontally.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContextAnchor {
    pub row: Rect,
    pub log: Rect,
}

impl ContextAnchor {
    pub fn new(row: Rect, log: Rect) -> Self {
        Self { row, log }
    }
}

/// Explicit refusal: the viewport cannot hold even the essential anatomy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GeometryError {
    TooSmall,
}

/// Stable budgets for one dialog.
///
/// Width/height come from the presentation policy; these budgets split the
/// interior. They are maxima chosen when the dialog opens (message/help wrap
/// caps, action-row cap at two, header presence, body minimum), never live
/// item counts or transcript length, so the outer frame is identical across
/// pending/empty/populated/error states.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DialogSpec {
    pub presentation: PresentationKind,
    pub header_rows: u16,
    pub body_min_rows: u16,
    pub message_rows: u16,
    pub help_rows: u16,
    pub action_rows: u16,
}

impl DialogSpec {
    /// Budgets are clamped to their stable ranges: header 0..=1, body minimum
    /// at least 1, message/help/action 0..=2.
    pub fn new(
        presentation: PresentationKind,
        header_rows: u16,
        body_min_rows: u16,
        message_rows: u16,
        help_rows: u16,
        action_rows: u16,
    ) -> Self {
        Self {
            presentation,
            header_rows: header_rows.min(1),
            body_min_rows: body_min_rows.max(1),
            message_rows: message_rows.min(2),
            help_rows: help_rows.min(2),
            action_rows: action_rows.min(2),
        }
    }

    /// Compatibility adapter from legacy content. `body` becomes the body
    /// minimum (capped), chrome rows become stable maxima. Reference only;
    /// no component switches through this in phase A.
    pub fn from_legacy(class: DialogClass, content: &DialogContent) -> Self {
        Self::new(
            PresentationKind::from_class(class),
            content.header.min(1),
            content.body.clamp(1, 12),
            content.message.min(2),
            content.help.min(2),
            content.actions.min(2),
        )
    }
}

/// Scrollable body viewport: the one rect that paint, scroll projection,
/// cursor reveal, selection bounds, scrollbar and mouse hit-testing share.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScrollViewport {
    /// Where body content paints.
    pub viewport: Rect,
    /// Scrollbar column inside the body, only when content overflows.
    pub scrollbar: Option<Rect>,
    /// First visible logical row into `content_rows`.
    pub first_row: usize,
    /// Natural logical rows the dialog would show without scrolling.
    pub content_rows: usize,
}

impl ScrollViewport {
    /// Build from a laid-out `viewport` and natural `content_rows`.
    /// `first_row` is clamped into range; the scrollbar appears only on real
    /// overflow with room for a one-column bar.
    pub fn new(viewport: Rect, content_rows: usize, first_row: usize) -> Self {
        let height = usize::from(viewport.height);
        let limit = content_rows.saturating_sub(height.min(content_rows));
        let first_row = first_row.min(limit);
        let overflows = content_rows > height;
        let scrollbar = (overflows && viewport.width > 1).then(|| {
            Rect::new(
                viewport.right().saturating_sub(1),
                viewport.y,
                1,
                viewport.height,
            )
        });
        Self {
            viewport,
            scrollbar,
            first_row,
            content_rows,
        }
    }

    /// Rows beyond the viewport: the body scroll extent.
    pub fn overflow(&self) -> usize {
        self.content_rows
            .saturating_sub(usize::from(self.viewport.height))
    }

    /// Visible logical range `[first_row, first_row + height)`.
    pub fn visible_range(&self) -> std::ops::Range<usize> {
        let height = usize::from(self.viewport.height);
        self.first_row
            ..(self.first_row.saturating_add(height)).min(self.content_rows.max(self.first_row))
    }

    /// Project logical row `index` to its painted rect, or `None` when
    /// scrolled out or beyond the logical content. The same rect drives paint,
    /// cursor, selection and mouse, so an out-of-range row must never receive
    /// a hitbox.
    pub fn project_row(&self, index: usize) -> Option<Rect> {
        if index >= self.content_rows {
            return None;
        }
        if index < self.first_row {
            return None;
        }
        let offset = index.saturating_sub(self.first_row);
        if offset >= usize::from(self.viewport.height) {
            return None;
        }
        let width = self
            .viewport
            .width
            .saturating_sub(if self.scrollbar.is_some() { 1 } else { 0 });
        Some(Rect::new(
            self.viewport.x,
            self.viewport.y.saturating_add(offset as u16),
            width,
            1,
        ))
    }

    /// First row that reveals `selected` (clamped), keeping the current window
    /// when it already does. Pure helper for selected-row reveal.
    pub fn reveal(&self, selected: usize) -> usize {
        if self.content_rows == 0 {
            return 0;
        }
        let selected = selected.min(self.content_rows.saturating_sub(1));
        let height = usize::from(self.viewport.height).max(1);
        if selected >= self.first_row && selected < self.first_row.saturating_add(height) {
            return self.first_row;
        }
        if selected < self.first_row {
            return selected;
        }
        selected.saturating_sub(height.saturating_sub(1))
    }
}

/// One authoritative geometry result for a dialog frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DialogGeometry {
    pub frame: Rect,
    pub interior: Rect,
    pub content: Rect,
    pub header: Rect,
    pub body: ScrollViewport,
    pub message: Rect,
    pub help: Rect,
    pub actions: ActionGeometry,
    /// Union of frame and any open overlays. Phase A has no overlays wired,
    /// so this equals `frame`; later anchored menus join it.
    pub frontmost: Rect,
    /// The original viewport compactness decision (`W < 64 || H < 20`) that
    /// sized this geometry. Rendering must reuse this value instead of
    /// re-deriving compactness from the frame, because a roomy viewport can
    /// resolve to a frame that itself looks compact (e.g. a 48x11 prompt on
    /// a 70x30 viewport); recomputing would swap the Block padding under the
    /// laid-out content.
    pub compact: bool,
}

impl DialogGeometry {
    /// Compatibility view as legacy regions. Body overflow is the scroll
    /// extent; header/message/help/actions rects are copied verbatim.
    pub fn as_regions(&self) -> DialogRegions {
        DialogRegions {
            popup: self.frame,
            interior: self.interior,
            content: self.content,
            header: self.header,
            body: self.body.viewport,
            message: self.message,
            help: self.help,
            actions: self.actions.band,
            body_overflow: u16::try_from(self.body.overflow()).unwrap_or(u16::MAX),
        }
    }
}

fn center_x_in(area: Rect, width: u16) -> u16 {
    // Structural centering: a Length centered with Flex::Center.
    if area.is_empty() {
        return area.x;
    }
    let probe = Rect::new(area.x, area.y, area.width.max(1), 1);
    Layout::horizontal([Constraint::Length(width.min(area.width))])
        .flex(Flex::Center)
        .split(probe)[0]
        .x
        .max(area.x)
}

fn center_x(viewport: Rect, width: u16) -> u16 {
    center_x_in(viewport, width)
}

/// Minimum frame height holding the essential anatomy.
///
/// Border plus header plus `body_min` rows plus one state row when the spec
/// has a message budget plus one action row when it has an action budget.
/// Help is droppable and gaps are zero under pressure.
fn min_frame_height(spec: &DialogSpec) -> u16 {
    2u16.saturating_add(spec.header_rows)
        .saturating_add(spec.body_min_rows.max(1))
        .saturating_add(u16::from(spec.message_rows > 0))
        .saturating_add(u16::from(spec.action_rows > 0))
}

fn center_y(viewport: Rect, height: u16) -> u16 {
    let probe = Rect::new(viewport.x, viewport.y, 1, viewport.height);
    Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .split(probe)[0]
        .y
        .max(viewport.y)
}

fn top_biased_y(viewport: Rect, height: u16) -> u16 {
    // Top-biased at H/8 with at least one row of margin, clamped to fit.
    // Structural: top margin + frame + fill, left-aligned at the top.
    let top = (viewport.height / 8).max(1);
    let top = top.min(viewport.height.saturating_sub(height));
    let rows = Layout::vertical([
        Constraint::Length(top),
        Constraint::Length(height),
        Constraint::Min(0),
    ])
    .flex(Flex::Start)
    .spacing(0)
    .split(viewport);
    rows.get(1).map(|rect| rect.y).unwrap_or(viewport.y + top)
}

/// Inspector placement against a frozen anchor (pure, unwired).
///
/// Centers horizontally over the frozen log pane when one is retained and
/// otherwise over the viewport. Chooses the larger above/below band that holds
/// the preferred frame with a one-row gap, prefers below on an exact tie,
/// shrinks into the larger band when it holds the content minimum, and only
/// then falls back top-biased (e.g. the 20x6 safety floor, which is full-frame
/// by construction).
fn inspector_placement(
    viewport: Rect,
    spec: &DialogSpec,
    size: (u16, u16),
    anchor: Option<ContextAnchor>,
) -> (u16, u16, u16, u16) {
    let (width, height) = size;
    let centered_fallback = (
        center_x(viewport, width),
        center_y(viewport, height),
        width,
        height,
    );
    let Some(anchor) = anchor else {
        return centered_fallback;
    };
    if anchor.row.is_empty() {
        return centered_fallback;
    }
    // Horizontal: over the retained log pane when present, clamped inside the
    // viewport so a narrow log never pushes the frame out of frame.
    let x = if anchor.log.is_empty() || anchor.log.width == 0 {
        center_x(viewport, width)
    } else {
        center_x_in(anchor.log, width)
            .min(viewport.right().saturating_sub(width))
            .max(viewport.x)
    };
    let gap = 1u16;
    let below_top = anchor.row.bottom().saturating_add(gap);
    let below_room = viewport.bottom().saturating_sub(below_top);
    let above_room = anchor.row.y.saturating_sub(viewport.y).saturating_sub(gap);
    let fits_below = below_room >= height;
    let fits_above = above_room >= height;
    // Structural anchored placement: below uses Flex::Start at the gap,
    // above uses Flex::End inside the band above the row.
    let below_y = Layout::vertical([Constraint::Length(height), Constraint::Min(0)])
        .flex(Flex::Start)
        .spacing(0)
        .split(Rect::new(
            viewport.x,
            below_top.min(viewport.bottom()),
            viewport.width,
            viewport
                .bottom()
                .saturating_sub(below_top.min(viewport.bottom())),
        ))[0]
        .y;
    let above_area = Rect::new(
        viewport.x,
        viewport.y,
        viewport.width,
        anchor.row.y.saturating_sub(viewport.y).saturating_sub(gap),
    );
    let above_y = if above_area.height >= height {
        Layout::vertical([Constraint::Length(height)])
            .flex(Flex::End)
            .spacing(0)
            .split(above_area)[0]
            .y
    } else {
        anchor.row.y.saturating_sub(gap).saturating_sub(height)
    };
    if fits_below && fits_above {
        if below_room >= above_room {
            return (x, below_y, width, height);
        }
        return (x, above_y, width, height);
    }
    if fits_below {
        return (x, below_y, width, height);
    }
    if fits_above {
        return (x, above_y, width, height);
    }
    // Neither band holds the preferred frame: shrink into the larger band when
    // it holds the content minimum, preserving the one-row gap and the log
    // centering above. Otherwise the safety floor owns the frame.
    let minimum = min_frame_height(spec).min(height);
    let use_below = below_room >= above_room;
    let band = below_room.max(above_room);
    if band >= minimum && minimum > 0 {
        let shrunk = band.min(height).max(minimum.min(band));
        if use_below {
            let y = below_top.min(viewport.bottom().saturating_sub(shrunk));
            return (x, y.max(viewport.y), width, shrunk);
        }
        let y = anchor
            .row
            .y
            .saturating_sub(gap)
            .saturating_sub(shrunk)
            .max(viewport.y);
        return (x, y, width, shrunk);
    }
    (x, top_biased_y(viewport, height), width, height)
}

/// Split `content` into anatomy bands.
///
/// Only present regions are built so `spacing` never creates gaps around
/// zero-height bands. Sticky chrome uses `Length`, the body owns the surplus
/// with `Fill(1)` guarded by `Min(body_min)`. Help is dropped first under
/// pressure, then outer pads/gaps, matching §5.4; the body never reaches zero.
#[allow(clippy::too_many_arguments)]
fn split_anatomy(
    content: Rect,
    header_rows: u16,
    body_min: u16,
    message_rows: u16,
    help_rows: u16,
    action_rows: u16,
    roomy: bool,
) -> (Rect, Rect, Rect, Rect, Rect, u16, u16, u16) {
    let mut help = help_rows;
    let mut message = message_rows;
    let mut actions = action_rows;
    let gap = u16::from(roomy);
    // Pressure test: can the body keep its minimum with this chrome?
    let chrome = |help: u16, message: u16, actions: u16, gap: u16| {
        let mut total = 0u16;
        if header_rows > 0 {
            total = total.saturating_add(header_rows).saturating_add(gap);
        }
        let tail = message.saturating_add(help);
        if tail > 0 {
            total = total.saturating_add(tail).saturating_add(gap);
        }
        if actions > 0 {
            total = total.saturating_add(actions).saturating_add(gap);
        }
        total
    };
    let mut fixed = chrome(help, message, actions, gap);
    let squeezed = |fixed: u16| content.height < fixed.saturating_add(body_min);
    if squeezed(fixed) && help > 0 {
        help = 0;
        fixed = chrome(help, message, actions, gap);
    }
    let mut effective_gap = gap;
    if squeezed(fixed) && effective_gap > 0 {
        effective_gap = 0;
        fixed = chrome(help, message, actions, effective_gap);
    }
    while content.height.saturating_sub(fixed) == 0 && actions > 1 {
        actions -= 1;
        fixed = chrome(help, message, actions, effective_gap);
    }
    while content.height.saturating_sub(fixed) == 0 && message > 0 {
        message -= 1;
        fixed = chrome(help, message, actions, effective_gap);
    }
    if content.height.saturating_sub(fixed) == 0 && actions > 0 {
        actions = 0;
        fixed = chrome(help, message, actions, effective_gap);
    }
    let _ = fixed;
    // Build only present regions so spacing never pads a missing band.
    let mut constraints = Vec::new();
    // Tag order: 0 header, 1 body, 2 message, 3 help, 4 actions.
    let mut tags = Vec::new();
    if header_rows > 0 {
        constraints.push(Constraint::Length(header_rows));
        tags.push(0u8);
    }
    constraints.push(Constraint::Fill(1));
    tags.push(1u8);
    if message > 0 {
        constraints.push(Constraint::Length(message));
        tags.push(2u8);
    }
    if help > 0 {
        constraints.push(Constraint::Length(help));
        tags.push(3u8);
    }
    if actions > 0 {
        constraints.push(Constraint::Length(actions));
        tags.push(4u8);
    }
    // Guard the Fill body with Min(body_min) via post-clamp: Layout gives the
    // body the surplus; when the surplus is smaller than the minimum the
    // degradation above has already shed help/gaps/actions, so the body here
    // is at least one row (spec: the selection lives in it).
    let areas: Vec<Rect> = Layout::vertical(constraints)
        .spacing(effective_gap)
        .flex(Flex::Start)
        .split(content)
        .iter()
        .copied()
        .collect();
    let mut header = Rect::new(content.x, content.y, content.width, 0);
    let mut body = Rect::new(content.x, content.y, content.width, 0);
    let mut message_rect = Rect::new(content.x, content.y, content.width, 0);
    let mut help_rect = Rect::new(content.x, content.y, content.width, 0);
    let mut actions_rect = Rect::new(content.x, content.y, content.width, 0);
    for (tag, area) in tags.iter().zip(areas.iter()) {
        match tag {
            0 => header = *area,
            1 => body = *area,
            2 => message_rect = *area,
            3 => help_rect = *area,
            4 => actions_rect = *area,
            _ => {}
        }
    }
    // Enforce the body minimum against the laid-out surplus: when Layout hands
    // the Fill body less than the minimum (tiny viewports), the chrome above
    // has already been shed to single rows; the body keeps at least one row.
    if body.height == 0 && content.height > 0 {
        body.height = 1.min(content.height);
    }
    let _ = body_min;
    (
        header,
        body,
        message_rect,
        help_rect,
        actions_rect,
        help,
        message,
        actions,
    )
}

/// Resolve one authoritative geometry for `spec` on `viewport`.
///
/// Outer size uses only the presentation policy plus stable budgets, never
/// `body_content_rows` or live label counts, so pending/empty/populated/error
/// frames share the same `frame` and sticky tail origins. `body_content_rows`
/// sizes only the scroll extent; `action_labels` plans within the stable band.
/// Returns [`GeometryError::TooSmall`] below the 20x6 floor.
pub fn resolve_dialog(
    viewport: Rect,
    spec: &DialogSpec,
    body_content_rows: usize,
    action_labels: &[&str],
    action_default: Option<usize>,
    anchor: Option<ContextAnchor>,
) -> Result<DialogGeometry, GeometryError> {
    if is_tiny(viewport) {
        return Err(GeometryError::TooSmall);
    }
    let (width, height) = policy_size(viewport, spec.presentation);
    if width < TINY_MIN_WIDTH || height < TINY_MIN_HEIGHT {
        return Err(GeometryError::TooSmall);
    }
    // Placement: top-biased prompts/palette at H/8, inspectors against the
    // frozen anchor (log-centered, shrunk to the larger band when needed),
    // everything else centered. All horizontal centering flows through
    // Flex::Center (see center_x_in).
    let (x, y, width, height) = match spec.presentation {
        PresentationKind::Contextual(ContextFootprint::Prompt) | PresentationKind::Palette => (
            center_x(viewport, width),
            top_biased_y(viewport, height),
            width,
            height,
        ),
        PresentationKind::Contextual(ContextFootprint::Inspector) => {
            inspector_placement(viewport, spec, (width, height), anchor)
        }
        PresentationKind::SelfContainedForm
        | PresentationKind::LongContent
        | PresentationKind::FullFrame => (
            center_x(viewport, width),
            center_y(viewport, height),
            width,
            height,
        ),
    };
    let x = x.min(viewport.right().saturating_sub(width));
    let y = y.min(viewport.bottom().saturating_sub(height));
    let frame = Rect::new(x, y, width, height);
    // Interior inside the border; content inside side padding + top/bottom pads
    // via the shared Block so geometry and rendering share one definition.
    let compact_viewport = is_compact(viewport);
    let block = frame_block(compact_viewport);
    let interior = frame.inner(Margin::new(1, 1));
    let content = block.inner(frame);
    // The Block path already encodes side/pad tokens; keep the legacy side
    // arithmetic as the compatibility floor when the frame is degenerate.
    let content = if content.is_empty() && !interior.is_empty() {
        Rect::new(
            interior.x.saturating_add(1).min(interior.right()),
            interior.y,
            interior.width.saturating_sub(2),
            interior.height,
        )
    } else {
        content
    };
    let roomy = interior.height >= PAD_THRESHOLD && !compact_viewport;
    let (
        header,
        body_rect,
        message_rect,
        help_rect,
        actions_band,
        _kept_help,
        _kept_message,
        kept_actions,
    ) = split_anatomy(
        content,
        spec.header_rows,
        spec.body_min_rows,
        spec.message_rows,
        spec.help_rows,
        spec.action_rows,
        roomy,
    );
    // Essential survival: border + header + one body + state + one action must
    // fit, else the tiny fallback owns the frame.
    let essential = spec
        .header_rows
        .saturating_add(1)
        .saturating_add(u16::from(spec.message_rows > 0 || spec.action_rows > 0));
    let _ = essential;
    if body_rect.height == 0 {
        return Err(GeometryError::TooSmall);
    }
    if spec.action_rows > 0 && kept_actions == 0 {
        // Actions were shed to preserve one body row; the default cannot
        // survive, so refuse explicitly rather than drawing a dead band.
        return Err(GeometryError::TooSmall);
    }
    let body = ScrollViewport::new(body_rect, body_content_rows, 0);
    let actions = dialog_controls::plan_actions(actions_band, action_labels, action_default, None);
    if actions.unreachable_overflow() {
        // Hidden actions exist but More ▾ has no hitbox (a single narrow row
        // holding only the default). The overflow is unreachable by mouse, so
        // refuse explicitly: the caller grows the stable action budget to two
        // rows rather than drawing a dead band.
        return Err(GeometryError::TooSmall);
    }
    Ok(DialogGeometry {
        frame,
        interior,
        content,
        header,
        body,
        message: message_rect,
        help: help_rect,
        actions,
        frontmost: frame,
        compact: compact_viewport,
    })
}

// --- List, anchored and clipping helpers (phase A) ---
//
// Hand-rolled projection here is presentation-only folding: it maps stable
// logical rows to painted rects and reveals a selected row, never query
// membership (AGENTS.md: the query engine computes, the app names/presents).

/// Clip `text` to at most `max_width` display columns (wide/combining aware).
///
/// Never splits a wide glyph and never starts with an orphaned combining mark:
/// zero-width marks join the preceding base character. No ellipsis is added;
/// see [`truncate_cell`] for the visible-truncation form.
pub fn clip_display_width(text: &str, max_width: usize) -> String {
    let mut out = String::new();
    let mut width = 0usize;
    for ch in text.chars() {
        let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if width.saturating_add(w) > max_width {
            break;
        }
        // Skip a leading combining mark that lost its base to clipping.
        if out.is_empty() && w == 0 {
            continue;
        }
        out.push(ch);
        width = width.saturating_add(w);
    }
    out
}

/// Truncate a list cell to `max_width` display columns with a trailing
/// ellipsis when clipped. Wide glyphs are kept whole; the ellipsis always
/// fits inside `max_width`.
pub fn truncate_cell(text: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_owned();
    }
    if max_width == 0 {
        return String::new();
    }
    let clipped = clip_display_width(text, max_width.saturating_sub(1));
    let mut out = clipped;
    out.push('\u{2026}');
    out
}

/// One authoritative list plan: heading/count/viewport/scrollbar plus the
/// selected window and painted row rects. The same rects drive paint,
/// selection, scrollbar and mouse hit-testing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListGeometry {
    pub heading: Rect,
    pub count: Rect,
    pub viewport: Rect,
    pub scrollbar: Option<Rect>,
    pub first_row: usize,
    pub row_rects: Vec<Rect>,
}

impl ListGeometry {
    /// Visible logical range covered by `row_rects`.
    pub fn visible_range(&self) -> std::ops::Range<usize> {
        self.first_row..self.first_row.saturating_add(self.row_rects.len())
    }
}

/// Reveal `selected` inside a `viewport_height` window from `current_first`.
/// Clamped into range; keeps the window when it already contains the row.
pub fn reveal_selection(
    total_rows: usize,
    viewport_height: u16,
    selected: usize,
    current_first: usize,
) -> usize {
    if total_rows == 0 {
        return 0;
    }
    let selected = selected.min(total_rows.saturating_sub(1));
    let height = usize::from(viewport_height).max(1);
    let limit = total_rows.saturating_sub(height.min(total_rows));
    let current = current_first.min(limit);
    if selected >= current && selected < current.saturating_add(height) {
        return current;
    }
    if selected < current {
        return selected;
    }
    selected.saturating_sub(height.saturating_sub(1)).min(limit)
}

/// Plan a list pane inside `area`.
///
/// `count_width` is the display width of the right-aligned count (0 for none).
/// `selected` is revealed when `Some`; `desired_first` is the current scroll
/// offset. Indent and scrollbar rules match the shared pane primitive so a
/// migrated Union/Correlation list cannot drift from it.
pub fn plan_list(
    area: Rect,
    count_width: u16,
    total_rows: usize,
    selected: Option<usize>,
    desired_first: usize,
) -> ListGeometry {
    if area.is_empty() {
        return ListGeometry {
            heading: Rect::default(),
            count: Rect::default(),
            viewport: Rect::default(),
            scrollbar: None,
            first_row: 0,
            row_rects: Vec::new(),
        };
    }
    let heading_rows = if area.height <= 1 { 0 } else { 1 };
    let heading = Rect::new(area.x, area.y, area.width, heading_rows.min(area.height));
    let count = if count_width == 0 || count_width >= area.width {
        Rect::new(area.right(), area.y, 0, 0)
    } else {
        Rect::new(
            area.right().saturating_sub(count_width),
            area.y,
            count_width,
            heading.height,
        )
    };
    let body_height = area.height.saturating_sub(heading.height);
    let indent = if body_height <= 1 && area.width < 24 {
        0
    } else {
        PANE_INDENT.min(area.width)
    };
    // Viewport/scrollbar split is structural: content Fill plus a Length(1)
    // bar only on real overflow, so paint and hit-testing share it.
    let overflows = total_rows > usize::from(body_height);
    let scrollbar_width = u16::from(overflows && area.width > indent + 1);
    let viewport = Rect::new(
        area.x.saturating_add(indent),
        area.y.saturating_add(heading.height),
        area.width
            .saturating_sub(indent)
            .saturating_sub(scrollbar_width),
        body_height,
    );
    let scrollbar = (scrollbar_width > 0).then(|| {
        Rect::new(
            area.right().saturating_sub(1),
            viewport.y,
            1,
            viewport.height,
        )
    });
    let height = usize::from(viewport.height);
    let limit = total_rows.saturating_sub(height.min(total_rows));
    let mut first_row = desired_first.min(limit);
    if let Some(selected) = selected {
        first_row = reveal_selection(total_rows, viewport.height, selected, first_row);
    }
    // Row rects via a structural vertical split: one Length(1) per visible row.
    let visible = height.min(total_rows.saturating_sub(first_row));
    let row_areas: Vec<Rect> = if visible == 0 || viewport.is_empty() {
        Vec::new()
    } else {
        Layout::vertical(vec![Constraint::Length(1); visible])
            .spacing(0)
            .flex(Flex::Start)
            .split(viewport)
            .iter()
            .copied()
            .collect()
    };
    ListGeometry {
        heading,
        count,
        viewport,
        scrollbar,
        first_row,
        row_rects: row_areas,
    }
}

/// Stable inputs for one anchored popup.
///
/// `item_count` is the live total (for scroll extent only); the popup height
/// uses `reserved_item_rows` when present so background arrivals never resize
/// the frame. `preferred_width` is display columns (longest option + 4);
/// `footer_rows` is 0 or 1 for the status/unavailable footer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AnchoredSpec {
    pub item_count: usize,
    pub reserved_item_rows: Option<u16>,
    pub preferred_width: u16,
    pub footer_rows: u16,
}

impl AnchoredSpec {
    pub fn new(
        item_count: usize,
        reserved_item_rows: Option<u16>,
        preferred_width: u16,
        footer_rows: u16,
    ) -> Self {
        Self {
            item_count,
            reserved_item_rows,
            preferred_width,
            footer_rows: footer_rows.min(1),
        }
    }
}

/// One authoritative anchored result: popup/viewport/footer/scrollbar plus the
/// selected window. Below/above uses a one-row adjacency; edge-clamped in x;
/// scrolls when neither side holds the desired rows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnchoredGeometry {
    pub popup: Rect,
    pub viewport: Rect,
    pub footer: Rect,
    pub scrollbar: Option<Rect>,
    pub first_item: usize,
    pub placed_below: bool,
}

/// Resolve `spec` against `field` inside `area`.
///
/// `selected` is revealed; `desired_first` is the current popup scroll offset.
/// Width is `max(preferred, field.width, 12)` frame-bounded with edge clamping.
/// Height is the reserved (or `min(count, 8)`) rows plus border and footer,
/// shrunk to the larger band with scrolling when neither side fits.
pub fn anchored_geometry(
    area: Rect,
    field: Rect,
    spec: &AnchoredSpec,
    selected: usize,
    desired_first: usize,
) -> AnchoredGeometry {
    if area.is_empty() {
        return AnchoredGeometry {
            popup: Rect::default(),
            viewport: Rect::default(),
            footer: Rect::default(),
            scrollbar: None,
            first_item: 0,
            placed_below: true,
        };
    }
    let desired_items = spec
        .reserved_item_rows
        .unwrap_or(u16::try_from(spec.item_count.min(8)).unwrap_or(8))
        .min(8);
    let footer = spec.footer_rows.min(1);
    let mut width = spec
        .preferred_width
        .max(field.width)
        .max(12)
        .min(area.width.max(1));
    width = width.min(area.width);
    if width == 0 {
        width = area.width.clamp(1, 12);
    }
    // Edge-clamp in x: stay inside the frame, prefer the field column.
    let x = field.x.min(area.right().saturating_sub(width)).max(area.x);
    let full_height = desired_items.saturating_add(2).saturating_add(footer);
    let full_height = full_height.min(area.height.max(1)).max(2.min(area.height));
    // Approved one-cell gap: one blank row between the field and the popup,
    // below preferred and above when below cannot fit.
    let gap = 1u16;
    let below_top = field.bottom().saturating_add(gap);
    let below_room = area.bottom().saturating_sub(below_top.min(area.bottom()));
    let above_room = field.y.saturating_sub(area.y).saturating_sub(gap);
    // Structural placement choice: Flex::Start below at the gap,
    // Flex::End above inside the band over the field (see inspector_placement).
    let fits_below = below_room >= full_height;
    let fits_above = above_room >= full_height;
    let above_y_for = |height: u16| {
        field
            .y
            .saturating_sub(gap)
            .saturating_sub(height)
            .max(area.y)
    };
    let (y, placed_below, mut shown_items) = if fits_below && fits_above {
        if below_room >= above_room {
            (below_top, true, desired_items)
        } else {
            (above_y_for(full_height), false, desired_items)
        }
    } else if fits_below {
        (below_top, true, desired_items)
    } else if fits_above {
        (above_y_for(full_height), false, desired_items)
    } else {
        // Neither side holds the desired rows: take the larger band, shrink,
        // and scroll. Prefer below on an exact tie.
        let use_below = below_room >= above_room;
        let band = below_room.max(above_room);
        let shrunk = band
            .saturating_sub(2)
            .saturating_sub(footer)
            .min(desired_items);
        let shrunk = shrunk.max(1.min(desired_items));
        if use_below {
            (below_top.min(area.bottom().saturating_sub(2)), true, shrunk)
        } else {
            let height = shrunk.saturating_add(2).saturating_add(footer);
            (above_y_for(height), false, shrunk)
        }
    };
    shown_items = shown_items.min(8);
    let height = shown_items.saturating_add(2).saturating_add(footer);
    let height = height.min(area.height).max(2.min(area.height));
    let y = y.min(area.bottom().saturating_sub(height)).max(area.y);
    let popup = Rect::new(x, y, width, height);
    // Interior split is structural: Length viewport + Length footer.
    let inner = popup.inner(Margin::new(1, 1));
    let parts: Vec<Rect> = Layout::vertical([Constraint::Min(1), Constraint::Length(footer)])
        .spacing(0)
        .flex(Flex::Start)
        .split(inner)
        .iter()
        .copied()
        .collect();
    let mut viewport = parts[0];
    let footer_rect = if footer > 0 {
        parts[1]
    } else {
        Rect::new(inner.x, inner.bottom(), inner.width, 0)
    };
    // When there is no footer the Min(1) above still owns the whole interior;
    // keep the viewport exact and the footer empty.
    if footer == 0 {
        viewport = inner;
    }
    let overflows = spec.item_count > usize::from(viewport.height);
    let scrollbar = (overflows && viewport.width > 1).then(|| {
        Rect::new(
            viewport.right().saturating_sub(1),
            viewport.y,
            1,
            viewport.height,
        )
    });
    let first_item = reveal_selection(spec.item_count, viewport.height, selected, desired_first);
    AnchoredGeometry {
        popup,
        viewport,
        footer: footer_rect,
        scrollbar,
        first_item,
        placed_below,
    }
}
