//! Shared dialog geometry: size classes, the single region layout function, the
//! backdrop scrim and the pane primitive.
//!
//! Implements `docs/dialog-system.md` §3 (anatomy), §4.1 (spacing scale),
//! §5 (sizing) and §6.2 (scrim). Dialogs supply content row counts and get back
//! rects; no dialog computes geometry of its own, so rendering, scrolling,
//! mouse hitboxes and text selection cannot drift apart.

use ratatui::{
    buffer::Buffer,
    layout::{Margin, Rect},
    style::{Color, Style},
};

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
    XL,
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
        // XL is always the full frame; the other classes collapse to it once
        // the terminal is compact, because margins cost cells they need.
        let full = area.width.saturating_sub(2);
        let width = if self == Self::XL || is_compact(area) {
            full
        } else {
            match self {
                Self::S => scaled(area.width, 60).clamp(48, 72),
                Self::M => scaled(area.width, 72).clamp(60, 96),
                Self::L => scaled(area.width, 86).clamp(72, 132),
                Self::P => scaled(area.width, 64).clamp(50, 92),
                Self::XL => full,
            }
        };
        width.min(area.width).max(MIN_DIALOG_WIDTH.min(area.width))
    }

    /// §5.1 maximum height. Actual height follows content (§5.2).
    pub fn max_height(self, area: Rect) -> u16 {
        let height = if is_compact(area) {
            match self {
                Self::S | Self::M => area.height.saturating_sub(2),
                Self::L | Self::XL | Self::P => area.height,
            }
        } else {
            match self {
                Self::S => 12,
                Self::M => area.height.saturating_sub(4),
                Self::L | Self::XL => area.height.saturating_sub(2),
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

/// §5.2. `content` is measured by the caller at `class.width(area) - 4`.
pub fn dialog_rect(area: Rect, class: DialogClass, content: &DialogContent) -> Rect {
    let width = class.width(area);
    let max_height = class.max_height(area);
    let height = content
        .interior_rows()
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

    // §5.4 degradation order: pads and gaps first, then help, then the body
    // scrolls. Each step is only taken when the previous one left too little.
    let mut pad = u16::from(interior.height >= PAD_THRESHOLD);
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
    // §5.4 degradation is driven by whether the content fits, not by a height
    // threshold: a dialog that is short because its content is short has no
    // pressure to relieve.
    let mut fixed = plan(pad, help, message, actions);
    let squeezed =
        |fixed: u16| interior.height < fixed.saturating_add(content.body.min(MIN_BODY_ROWS));
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
    while interior.height.saturating_sub(fixed) == 0 && actions > 1 {
        actions -= 1;
        fixed = plan(pad, help, message, actions);
    }
    while interior.height.saturating_sub(fixed) == 0 && message > 0 {
        message -= 1;
        fixed = plan(pad, help, message, actions);
    }
    if interior.height.saturating_sub(fixed) == 0 && actions > 0 {
        actions = 0;
        fixed = plan(pad, help, message, actions);
    }
    let gap = pad;
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

/// §6.2. Restyle the workspace behind an open dialog: muted foreground, no
/// modifiers, background untouched. A style pass over the buffer, not a widget:
/// it moves no cell and changes no hit region.
pub fn scrim(buffer: &mut Buffer, area: Rect, theme: Theme) {
    let area = area.intersection(buffer.area);
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            let cell = &mut buffer[(x, y)];
            let background = cell.style().bg.unwrap_or(theme.base_bg);
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
