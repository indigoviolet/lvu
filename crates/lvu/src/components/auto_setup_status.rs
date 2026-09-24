//! Automatic-setup inspection with explicit review (`docs/dialog-system.md` §12.23).
//!
//! The non-modal status line is intentionally terse: it has to coexist with
//! source health and ordinary log navigation. This inspector is the durable
//! answer to "what is it doing?": it puts the existing log first, then the
//! setup operation, the Paseo session (if it has been created), and the
//! diagnostic.
//!
//! Review-before-apply: a schema-checked proposal waits in
//! [`crate::auto_setup::AutoSetupStage::Review`] with its full bounded summary
//! (expressions, pins, colours, grouping and roles). Nothing is applied until
//! the explicit Apply action runs; Close (the safe default, including Enter
//! and Esc) preserves the pending proposal. Wrapping and scrolling here are
//! presentation-only folding: they decide which summary rows share the body
//! viewport, never query membership (AGENTS.md: the query engine computes, the
//! app names and presents).

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, MouseButton, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
};

use crate::app::Action;
use crate::auto_setup::{AutoSetupStage, AutoSetupStatus};
use crate::component::{Component, Ctx, Event, Outcome, RenderCtx, Surface};
use crate::dialog_controls::{ButtonRole, DialogStyles, render_role_button, stable_action_rows};
use crate::dialog_layout::{
    ContextFootprint, DialogSpec, PresentationKind, ScrollViewport, policy_size,
};
use crate::ui::render_help_text;

const ANALYZE_LABEL: &str = "&Analyze again";
/// `p` keeps this row free of duplicate mnemonics: `a` already reviews again
/// and `c` closes. Bare `p` works when no text field owns input; Alt-P always
/// works (§8.10).
const APPLY_LABEL: &str = "A&pply";
const CLOSE_LABEL: &str = "&Close";

/// Stable maximum action set, so the outer frame is identical whether or not a
/// proposal is waiting for review. The body owns the surplus via the shared
/// scroll viewport; live proposal length sizes only the scroll extent.
const MAX_ACTION_LABELS: [&str; 3] = [ANALYZE_LABEL, APPLY_LABEL, CLOSE_LABEL];

/// The inspector uses a contextual footprint. The four semantic rows are
/// stable whether the session is waiting, running, reviewing, completed or
/// unavailable; only their values change. The action band keeps the stable
/// maximum so pending/empty/populated frames share one outer rect.
pub fn auto_setup_status_spec(area: Rect) -> DialogSpec {
    let (policy_w, _) = policy_size(
        area,
        PresentationKind::Contextual(ContextFootprint::Inspector),
    );
    let estimate = policy_w.saturating_sub(4).max(1);
    let action_rows = stable_action_rows(estimate, &MAX_ACTION_LABELS).max(1);
    DialogSpec::new(
        PresentationKind::Contextual(ContextFootprint::Inspector),
        0,
        5,
        0,
        2,
        action_rows,
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AutoSetupStatusHit {
    Analyze,
    Apply,
    Close,
    Body,
}

#[derive(Clone, Debug, Default)]
struct Geometry {
    body: Rect,
    analyze: Option<Rect>,
    apply: Option<Rect>,
    close: Option<Rect>,
}

#[derive(Debug, Default)]
pub struct AutoSetupStatusDialog {
    open: bool,
    status: Option<AutoSetupStatus>,
    /// Full bounded review text for the pending proposal, when the executable
    /// has queued one. Stored separately from `status` so Close (and Esc)
    /// preserves the candidate across reopen: only an explicit
    /// `set_proposal_summary` replaces or clears it.
    proposal_summary: Option<String>,
    scroll: usize,
    scroll_limit: usize,
    geometry: Geometry,
    surface: Surface,
}

impl AutoSetupStatusDialog {
    /// Running work may acquire a Paseo session after the dialog opens. The
    /// shell updates this snapshot from its one authoritative status writer.
    /// The pending review text is preserved: a status update never invents or
    /// discards a proposal on its own.
    pub fn update(&mut self, status: AutoSetupStatus) {
        if self.open {
            self.status = Some(status);
        }
    }

    /// Store (or clear with `None`) the full bounded proposal summary the
    /// primary queues alongside `AutoSetupStage::Review`. Resets the body
    /// scroll so a fresh proposal opens at its top; preserving the old offset
    /// would leave a new candidate scrolled into its middle.
    pub fn set_proposal_summary(&mut self, summary: Option<String>) {
        if self.proposal_summary != summary {
            self.proposal_summary = summary;
            self.scroll = 0;
            self.scroll_limit = 0;
        }
    }

    pub fn proposal_summary(&self) -> Option<&str> {
        self.proposal_summary.as_deref()
    }

    /// The agreed integration gate: Apply is shown and enabled only while the
    /// lifecycle is waiting for review **and** a summary is present. Every
    /// other stage — sampling, analyzing, validating, applying, unavailable,
    /// applied — keeps the inspector read-only with Analyze again + Close.
    fn is_reviewable(&self) -> bool {
        self.status.as_ref().is_some_and(|status| {
            status.stage == AutoSetupStage::Review
                && self
                    .proposal_summary
                    .as_ref()
                    .is_some_and(|s| !s.is_empty())
        })
    }

    fn action_labels_vec(&self) -> Vec<&'static str> {
        if self.is_reviewable() {
            vec![ANALYZE_LABEL, APPLY_LABEL, CLOSE_LABEL]
        } else {
            vec![ANALYZE_LABEL, CLOSE_LABEL]
        }
    }

    fn close(&mut self) -> Outcome {
        // Safe default: closing never applies and never discards the pending
        // proposal. Reopening the inspector shows the same candidate.
        self.open = false;
        Outcome::Close
    }

    fn analyze(&mut self) -> Outcome {
        // Explicit retry: close the inspector and queue one fresh bounded
        // request. The pending summary stays stored but is gated hidden until
        // the new lifecycle reaches Review with its own summary.
        self.open = false;
        Outcome::Legacy(Action::AnalyzeAutoSetup)
    }

    fn apply(&mut self) -> Outcome {
        // Explicit review acceptance. The primary owns the pending proposal,
        // staleness fences and coordinator wiring; this only emits the agreed
        // action. Closing here keeps the behavior consistent with Analyze:
        // progress (applying/applied/unavailable) surfaces in the footer and
        // on reopen, never by stealing focus.
        self.open = false;
        Outcome::Legacy(Action::ApplyAutoSetup)
    }

    fn scroll_body(&mut self, delta: i32) {
        if self.scroll_limit == 0 {
            self.scroll = 0;
            return;
        }
        let next = self.scroll.saturating_add_signed(delta as isize);
        self.scroll = next.min(self.scroll_limit);
    }

    fn key(&mut self, key: KeyEvent) -> Outcome {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return Outcome::Ignored;
        }
        match key.code {
            // Close is the default safe action. The visible `A`/`P` mnemonics
            // are resolved by the shell before this key handler.
            KeyCode::Enter => self.close(),
            KeyCode::Up => {
                self.scroll_body(-1);
                Outcome::Consumed
            }
            KeyCode::Down => {
                self.scroll_body(1);
                Outcome::Consumed
            }
            _ => Outcome::Ignored,
        }
    }
}

fn contains(area: Rect, point: (u16, u16)) -> bool {
    point.0 >= area.x && point.0 < area.right() && point.1 >= area.y && point.1 < area.bottom()
}

fn row(label: &str, value: &str, _width: u16, styles: DialogStyles) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label}: "), styles.label),
        Span::styled(value.to_owned(), styles.description),
    ])
}

impl Component for AutoSetupStatusDialog {
    type Hit = AutoSetupStatusHit;
    type Open = AutoSetupStatus;

    fn open(&mut self, status: AutoSetupStatus, _ctx: &mut Ctx<'_>) {
        self.open = true;
        self.status = Some(status);
        // Preserve the pending proposal across Close/reopen; show its top.
        self.scroll = 0;
        self.geometry = Geometry::default();
        self.surface = Surface::default();
    }

    fn handle(&mut self, event: Event<AutoSetupStatusHit>, _ctx: &mut Ctx<'_>) -> Outcome {
        match event {
            Event::Key(key) => self.key(key),
            Event::Mouse { kind, hit, .. } => match (kind, hit) {
                (MouseEventKind::Down(MouseButton::Left), Some(AutoSetupStatusHit::Analyze)) => {
                    self.analyze()
                }
                (MouseEventKind::Down(MouseButton::Left), Some(AutoSetupStatusHit::Apply)) => {
                    if self.is_reviewable() {
                        self.apply()
                    } else {
                        Outcome::Consumed
                    }
                }
                (MouseEventKind::Down(MouseButton::Left), Some(AutoSetupStatusHit::Close)) => {
                    self.close()
                }
                (MouseEventKind::ScrollUp, _) => {
                    self.scroll_body(-1);
                    Outcome::Consumed
                }
                (MouseEventKind::ScrollDown, _) => {
                    self.scroll_body(1);
                    Outcome::Consumed
                }
                _ => Outcome::Consumed,
            },
            Event::Dismiss => self.close(),
            Event::Command(_) | Event::Paste(_) | Event::View(_) | Event::Resize => {
                Outcome::Ignored
            }
        }
    }

    fn action_labels(&self, _ctx: &Ctx<'_>) -> Vec<&'static str> {
        self.action_labels_vec()
    }

    fn press_action(&mut self, index: usize, _ctx: &mut Ctx<'_>) -> Outcome {
        if self.is_reviewable() {
            match index {
                0 => self.analyze(),
                1 => self.apply(),
                2 => self.close(),
                _ => Outcome::Ignored,
            }
        } else {
            match index {
                0 => self.analyze(),
                1 => self.close(),
                _ => Outcome::Ignored,
            }
        }
    }

    fn surface(&self) -> Surface {
        self.surface
    }

    fn hit(&self, point: (u16, u16)) -> Option<AutoSetupStatusHit> {
        if self
            .geometry
            .analyze
            .is_some_and(|rect| contains(rect, point))
        {
            Some(AutoSetupStatusHit::Analyze)
        } else if self
            .geometry
            .apply
            .is_some_and(|rect| contains(rect, point))
        {
            Some(AutoSetupStatusHit::Apply)
        } else if self
            .geometry
            .close
            .is_some_and(|rect| contains(rect, point))
        {
            Some(AutoSetupStatusHit::Close)
        } else {
            contains(self.geometry.body, point).then_some(AutoSetupStatusHit::Body)
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx<'_>) -> Surface {
        use crate::dialog_layout::resolve_dialog;

        let reviewable = self.is_reviewable();
        let labels = self.action_labels_vec();
        // Close stays the safe default in both rows: Enter/Esc never applies.
        let default = Some(labels.len().saturating_sub(1));
        let spec = auto_setup_status_spec(area);
        let Ok(geometry) = resolve_dialog(area, &spec, 1, &labels, default, None) else {
            self.geometry = Geometry::default();
            self.scroll_limit = 0;
            self.surface = Surface::default();
            return self.surface;
        };
        crate::ui::render_responsive_frame(
            frame,
            &geometry,
            "Automatic setup status",
            ctx.active,
            ctx.theme,
        );
        let mut surface = Surface {
            popup: geometry.frontmost,
            interior: geometry.interior,
            caret: None,
            scrollable: false,
            text_focus: false,
        };
        self.surface = surface;
        self.geometry = Geometry {
            body: geometry.body.viewport,
            ..Geometry::default()
        };
        let Some(status) = self.status.clone() else {
            return surface;
        };
        if geometry.content.width == 0 || !ctx.active {
            return surface;
        }

        let styles = DialogStyles::new(ctx.theme);
        let session = status
            .session_id
            .as_deref()
            .unwrap_or("creating a Paseo conversation");
        let detail = if status.detail.is_empty() {
            "waiting for a lifecycle update"
        } else {
            status.detail.as_str()
        };
        let body_width = geometry.body.viewport.width;
        let mut lines = vec![
            row("Log", &status.object_name, body_width, styles),
            row(
                "Operation",
                &format!("automatic setup: {}", status.stage.label()),
                body_width,
                styles,
            ),
            row("Paseo session", session, body_width, styles),
            row("Detail", detail, body_width, styles),
            Line::styled(
                "No log definition changes until you review a proposed Enhanced view.",
                styles.description,
            ),
        ];
        if reviewable {
            lines.push(Line::styled(
                "Proposed Enhanced view (review before apply):",
                styles.label,
            ));
            if let Some(summary) = &self.proposal_summary {
                // FULL bounded candidate: every summary line is kept and
                // wrapped, never truncated. Bounds come from the wire proposal;
                // scrolling is the only folding here.
                for summary_line in summary.split('\n') {
                    lines.push(Line::styled(summary_line.to_owned(), styles.description));
                }
            }
        } else if status.stage == AutoSetupStage::Review {
            lines.push(Line::styled(
                "Proposal ready; waiting for its review summary.",
                styles.description,
            ));
        }
        // Shared scroll viewport: the same rects drive paint, scrollbar and
        // mouse. The outer frame above is policy-only, so pending/empty/review
        // frames share it and only this extent grows with proposal length.
        let probe = Paragraph::new(lines.clone()).wrap(Wrap { trim: false });
        // `line_count` counts wrapped rows at this width; the scroll viewport
        // then reserves the scrollbar column itself on real overflow.
        let initial_rows = probe.line_count(body_width.max(1));
        let text_width = body_width.saturating_sub(u16::from(
            initial_rows > usize::from(geometry.body.viewport.height) && body_width > 1,
        ));
        // Recount at the width actually painted: reserving the scrollbar can
        // wrap another line, which must remain reachable at the bottom.
        let wrapped = probe.line_count(text_width.max(1));
        let viewport = ScrollViewport::new(geometry.body.viewport, wrapped, self.scroll);
        let limit = wrapped.saturating_sub(usize::from(viewport.viewport.height));
        self.scroll = self.scroll.min(limit);
        self.scroll_limit = limit;
        let scrolled = ScrollViewport::new(geometry.body.viewport, wrapped, self.scroll);
        let bar_w = u16::from(scrolled.scrollbar.is_some());
        let text_area = Rect::new(
            scrolled.viewport.x,
            scrolled.viewport.y,
            scrolled.viewport.width.saturating_sub(bar_w),
            scrolled.viewport.height,
        );
        if !text_area.is_empty() {
            frame.render_widget(
                Paragraph::new(lines)
                    .wrap(Wrap { trim: false })
                    .scroll((scrolled.first_row.min(u16::MAX as usize) as u16, 0)),
                text_area,
            );
        }
        if let Some(bar) = scrolled.scrollbar {
            crate::ui::render_scrollbar(
                frame,
                bar,
                scrolled.first_row,
                limit,
                ctx.theme,
                ctx.ascii,
            );
            surface.scrollable = true;
        }
        let help = if reviewable {
            "Up/Down scrolls the full proposal. Apply installs Enhanced; Esc keeps it for later."
        } else {
            "Analyze again starts a bounded fresh review. Esc closes this inspector."
        };
        render_help_text(frame, geometry.help, help, ctx.theme);
        let mut next = Geometry {
            body: geometry.body.viewport,
            ..Geometry::default()
        };
        for (index, rect) in &geometry.actions.buttons {
            let role = if geometry.actions.default == Some(*index) {
                ButtonRole::Default
            } else {
                ButtonRole::Normal
            };
            if let Some(label) = labels.get(*index) {
                render_role_button(frame, *rect, label, role, false, ctx.theme);
            }
            // `geometry.actions.buttons` carries original label indices, so a
            // three-button review row maps 0/1/2 to Analyze/Apply/Close while
            // the two-button row maps 0/1 to Analyze/Close.
            if reviewable {
                match *index {
                    0 => next.analyze = Some(*rect),
                    1 => next.apply = Some(*rect),
                    2 => next.close = Some(*rect),
                    _ => {}
                }
            } else {
                match *index {
                    0 => next.analyze = Some(*rect),
                    1 => next.close = Some(*rect),
                    _ => {}
                }
            }
        }
        // Overflow actions (if a narrow band ever hides one behind More) keep
        // their keyboard mnemonics via press_action even without a hitbox.
        self.geometry = next;
        self.surface = surface;
        surface
    }
}
