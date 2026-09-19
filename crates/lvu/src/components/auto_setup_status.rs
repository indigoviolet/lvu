//! Read-only automatic-setup inspection (`docs/dialog-system.md` §12.23).
//!
//! The non-modal status line is intentionally terse: it has to coexist with
//! source health and ordinary log navigation. This inspector is the durable
//! answer to "what is it doing?": it puts the existing log first, then the
//! setup operation, the Paseo session (if it has been created), and the
//! diagnostic. Nothing here applies a proposal or changes capture bytes.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, MouseButton, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
};

use crate::app::Action;
use crate::auto_setup::AutoSetupStatus;
use crate::component::{Component, Ctx, Event, Outcome, RenderCtx, Surface};
use crate::dialog_controls::{ButtonRole, DialogStyles, render_role_button};
use crate::dialog_layout::{ContextFootprint, DialogSpec, PresentationKind};
use crate::ui::{render_help_text, truncated};

const ANALYZE_LABEL: &str = "&Analyze again";
const CLOSE_LABEL: &str = "&Close";

/// The inspector uses a fixed medium footprint. The four semantic rows are
/// stable whether the session is waiting, running, completed or unavailable;
/// only their values change.
pub fn auto_setup_status_spec() -> DialogSpec {
    DialogSpec::new(
        PresentationKind::Contextual(ContextFootprint::Inspector),
        0,
        5,
        0,
        2,
        1,
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AutoSetupStatusHit {
    Analyze,
    Close,
    Body,
}

#[derive(Clone, Debug, Default)]
struct Geometry {
    body: Rect,
    analyze: Option<Rect>,
    close: Option<Rect>,
}

#[derive(Debug, Default)]
pub struct AutoSetupStatusDialog {
    open: bool,
    status: Option<AutoSetupStatus>,
    geometry: Geometry,
    surface: Surface,
}

impl AutoSetupStatusDialog {
    /// Running work may acquire a Paseo session after the dialog opens. The
    /// shell updates this snapshot from its one authoritative status writer.
    pub fn update(&mut self, status: AutoSetupStatus) {
        if self.open {
            self.status = Some(status);
        }
    }

    fn close(&mut self) -> Outcome {
        self.open = false;
        Outcome::Close
    }

    fn key(&mut self, key: KeyEvent) -> Outcome {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return Outcome::Ignored;
        }
        match key.code {
            // Close is the default safe action. The visible `A` mnemonic is
            // resolved by the shell before this key handler.
            KeyCode::Enter => self.close(),
            _ => Outcome::Ignored,
        }
    }
}

fn contains(area: Rect, point: (u16, u16)) -> bool {
    point.0 >= area.x && point.0 < area.right() && point.1 >= area.y && point.1 < area.bottom()
}

fn row(label: &str, value: &str, width: u16, styles: DialogStyles) -> Line<'static> {
    let available = usize::from(width.saturating_sub(label.len() as u16 + 2)).max(1);
    Line::from(vec![
        Span::styled(format!("{label}: "), styles.label),
        Span::styled(truncated(value, available), styles.description),
    ])
}

impl Component for AutoSetupStatusDialog {
    type Hit = AutoSetupStatusHit;
    type Open = AutoSetupStatus;

    fn open(&mut self, status: AutoSetupStatus, _ctx: &mut Ctx<'_>) {
        self.open = true;
        self.status = Some(status);
        self.geometry = Geometry::default();
        self.surface = Surface::default();
    }

    fn handle(&mut self, event: Event<AutoSetupStatusHit>, _ctx: &mut Ctx<'_>) -> Outcome {
        match event {
            Event::Key(key) => self.key(key),
            Event::Mouse { kind, hit, .. } => match (kind, hit) {
                (MouseEventKind::Down(MouseButton::Left), Some(AutoSetupStatusHit::Analyze)) => {
                    self.open = false;
                    Outcome::Legacy(Action::AnalyzeAutoSetup)
                }
                (MouseEventKind::Down(MouseButton::Left), Some(AutoSetupStatusHit::Close)) => {
                    self.close()
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
        vec![ANALYZE_LABEL, CLOSE_LABEL]
    }

    fn press_action(&mut self, index: usize, _ctx: &mut Ctx<'_>) -> Outcome {
        match index {
            0 => {
                self.open = false;
                Outcome::Legacy(Action::AnalyzeAutoSetup)
            }
            1 => self.close(),
            _ => Outcome::Ignored,
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

        let labels = [ANALYZE_LABEL, CLOSE_LABEL];
        let Ok(geometry) =
            resolve_dialog(area, &auto_setup_status_spec(), 5, &labels, Some(1), None)
        else {
            self.geometry = Geometry::default();
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
        let surface = Surface {
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
        let Some(status) = &self.status else {
            return surface;
        };
        if geometry.content.width == 0 || !ctx.active {
            return surface;
        }

        let session = status
            .session_id
            .as_deref()
            .unwrap_or("creating a Paseo conversation");
        let detail = if status.detail.is_empty() {
            "waiting for a lifecycle update"
        } else {
            status.detail.as_str()
        };
        let rows = vec![
            row(
                "Log",
                &status.object_name,
                geometry.body.viewport.width,
                DialogStyles::new(ctx.theme),
            ),
            row(
                "Operation",
                &format!("automatic setup: {}", status.stage.label()),
                geometry.body.viewport.width,
                DialogStyles::new(ctx.theme),
            ),
            row(
                "Paseo session",
                session,
                geometry.body.viewport.width,
                DialogStyles::new(ctx.theme),
            ),
            row(
                "Detail",
                detail,
                geometry.body.viewport.width,
                DialogStyles::new(ctx.theme),
            ),
            Line::styled(
                "No log definition changes until you review a proposed Enhanced view.",
                DialogStyles::new(ctx.theme).description,
            ),
        ];
        frame.render_widget(
            Paragraph::new(rows).wrap(Wrap { trim: false }),
            geometry.body.viewport,
        );
        render_help_text(
            frame,
            geometry.help,
            "Analyze again starts a bounded fresh review. Esc closes this inspector.",
            ctx.theme,
        );
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
            render_role_button(frame, *rect, labels[*index], role, false, ctx.theme);
            match *index {
                0 => next.analyze = Some(*rect),
                1 => next.close = Some(*rect),
                _ => {}
            }
        }
        self.geometry = next;
        surface
    }
}
