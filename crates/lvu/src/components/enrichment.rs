//! The enrichment step list — layer one of the enrichment stack — converted to
//! the component contract per `docs/component-model.md` §6.3 step 13.
//!
//! Everything this layer edits is view-owned (§2.5): `ViewState.enrichments`
//! is the accepted chain, `ViewState.enrichment` the shared editor state,
//! `enrichment_selected` and `enrichment_control` the cursor into them. None of
//! it is cached here, so a query completion landing in `ViewState` reaches the
//! screen with no event, and an unfinished edit survives a restart because it
//! was never dialog state to begin with.
//!
//! The two children it reaches differ, and the difference is the whole of
//! §6.5's step 13 note:
//!
//! * **The step editor is a real child.** `Add`/`Edit` return
//!   `OpenChild(Open::EnrichmentStep { .. })`; this layer keeps rendering its
//!   frame and title under the child's extra scrim, and the child's `Close`
//!   drops the user back onto the list with the selection intact.
//! * **External command is not.** It draws no parent today and its Escape goes
//!   to the workspace, not to this list, so it is reached by `Replace` and
//!   closes to the base. Making it a child would have been a visible delta.

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::{
    Frame,
    layout::Rect,
    style::Modifier,
    text::Line,
    widgets::{Paragraph, Wrap},
};
use unicode_width::UnicodeWidthStr;

use crate::app::{EnrichmentControl as Control, Views};
use crate::command_palette::CommandId;
use crate::component::{
    CommandEntry, CommandSpec, Component, Ctx, Event, Open, Outcome, RenderCtx, Surface,
};
use crate::dialog_controls::{DialogStyles, button_layout};
use crate::text_edit::TextTarget;
use crate::ui::{
    MessageState, class_l_popup, class_l_width, clear_themed, dialog_regions, message_rows,
    packed_button_rows, render_dialog_frame, render_enrichment_button, render_message,
    render_pane_heading, render_scrollbar, step_summary,
};

/// Everything the list draws that can be clicked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnrichmentHit {
    /// A step row, by index into the accepted chain.
    Row(usize),
    Control(Control),
    Body,
}

/// Recorded by `render`, consumed by `hit()` (§5.1). These were
/// `HitRegions::{enrichment_rows, enrichment_controls}`.
#[derive(Clone, Debug, Default)]
struct EnrichmentGeometry {
    rows: Vec<(Rect, usize)>,
    controls: Vec<(Rect, Control)>,
}

#[derive(Debug, Default)]
pub struct EnrichmentDialog {
    open: bool,
    geometry: EnrichmentGeometry,
    surface: Surface,
}

impl EnrichmentDialog {
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// The drawn button rects, which the shell's tests assert stay inside the
    /// modal bound (§5.1).
    pub fn control_rects(&self) -> &[(Rect, Control)] {
        &self.geometry.controls
    }

    pub fn row_rects(&self) -> &[(Rect, usize)] {
        &self.geometry.rows
    }

    fn record(&mut self, geometry: EnrichmentGeometry, surface: Surface) -> Surface {
        self.geometry = geometry;
        self.surface = surface;
        surface
    }

    /// Palette availability is `self.open`, and the palette catalog is built
    /// without a `Ctx`; this is how a test puts the layer in the open state
    /// without a shell.
    pub fn open_for_test(&mut self) {
        self.open = true;
    }

    fn control(&self, ctx: &Ctx<'_>) -> Option<Control> {
        ctx.views.active().map(|state| state.enrichment_control)
    }

    /// Moving the focus ring over the four buttons and the list.
    fn move_control(&mut self, delta: i32, ctx: &mut Ctx<'_>) -> Outcome {
        if let Some(state) = ctx.views.active_mut() {
            let index = Control::ALL
                .iter()
                .position(|control| *control == state.enrichment_control)
                .unwrap_or(0);
            state.enrichment_control =
                Control::ALL[(index as i32 + delta).rem_euclid(Control::ALL.len() as i32) as usize];
        }
        Outcome::Consumed
    }

    fn move_selection(&mut self, delta: i32, ctx: &mut Ctx<'_>) -> Outcome {
        if let Some(state) = ctx.views.active_mut()
            && !state.enrichments.is_empty()
        {
            state.enrichment_selected = (state.enrichment_selected as i32 + delta)
                .rem_euclid(state.enrichments.len() as i32)
                as usize;
        }
        Outcome::Consumed
    }

    /// `Add` opens the child with no stage; `Edit` with the selected one.
    /// Refusing to open on an empty list is a notice, not a silent no-op —
    /// that wording was `App`'s `action_notice` and is now `ctx.notice`.
    fn edit_selected(&mut self, ctx: &mut Ctx<'_>) -> Outcome {
        let stage = ctx
            .views
            .active()
            .and_then(|state| state.enrichments.get(state.enrichment_selected).cloned());
        match stage {
            Some(stage) => Outcome::OpenChild(Open::EnrichmentStep {
                editing: Some(stage.id),
            }),
            None => {
                ctx.notice("no enrichment step is selected; use Add to create one");
                Outcome::Consumed
            }
        }
    }

    fn remove_selected(&mut self, ctx: &mut Ctx<'_>) -> Outcome {
        remove_selected_stage(ctx);
        Outcome::Consumed
    }

    /// Activating whatever the focus ring is on. `Steps` is the list itself,
    /// where Enter means "open the selected step".
    fn activate(&mut self, control: Control, ctx: &mut Ctx<'_>) -> Outcome {
        match control {
            Control::Steps => self.edit_selected(ctx),
            Control::Add => Outcome::OpenChild(Open::EnrichmentStep { editing: None }),
            Control::Edit => self.edit_selected(ctx),
            Control::Remove => self.remove_selected(ctx),
            // §6.5: `Replace`, not `OpenChild`. See the module doc.
            Control::ExternalCommand => Outcome::Replace(Open::ExternalCommand),
        }
    }

    /// Clicking or shortcutting a button both focuses it and fires it, which is
    /// what `Action::FocusEnrichmentControl` did.
    fn focus_and_activate(&mut self, control: Control, ctx: &mut Ctx<'_>) -> Outcome {
        if let Some(state) = ctx.views.active_mut() {
            state.enrichment_control = control;
        }
        match control {
            Control::Steps => Outcome::Consumed,
            other => self.activate(other, ctx),
        }
    }

    fn key(&mut self, key: KeyEvent, ctx: &mut Ctx<'_>) -> Outcome {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return Outcome::Ignored;
        }
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        // The four accelerators fire their verb without moving the focus ring,
        // which is what the `Action::AddEnrichment` family did; only a click
        // both focuses and fires.
        match key.code {
            KeyCode::Char('a') | KeyCode::Char('A') if alt => self.activate(Control::Add, ctx),
            KeyCode::Char('e') | KeyCode::Char('E') if alt => self.activate(Control::Edit, ctx),
            KeyCode::Char('r') | KeyCode::Char('R') if alt => self.activate(Control::Remove, ctx),
            KeyCode::Char('c') | KeyCode::Char('C') if alt => {
                self.activate(Control::ExternalCommand, ctx)
            }
            KeyCode::Tab => self.move_control(
                if key.modifiers.contains(KeyModifiers::SHIFT) {
                    -1
                } else {
                    1
                },
                ctx,
            ),
            KeyCode::BackTab => self.move_control(-1, ctx),
            // The list has no overflowing pane, so Up/Down always move its
            // selection — which is what `ModalVertical` did with a zero scroll
            // limit, whichever button held the focus ring.
            KeyCode::Up => self.move_selection(-1, ctx),
            KeyCode::Down => self.move_selection(1, ctx),
            KeyCode::Enter => match self.control(ctx) {
                Some(control) => self.activate(control, ctx),
                None => Outcome::Consumed,
            },
            _ => Outcome::Ignored,
        }
    }

    fn mouse(
        &mut self,
        kind: MouseEventKind,
        hit: Option<EnrichmentHit>,
        ctx: &mut Ctx<'_>,
    ) -> Outcome {
        match kind {
            MouseEventKind::Down(MouseButton::Left) => match hit {
                Some(EnrichmentHit::Control(control)) => self.focus_and_activate(control, ctx),
                Some(EnrichmentHit::Row(index)) => {
                    if let Some(state) = ctx.views.active_mut() {
                        state.enrichment_selected = index;
                        state.enrichment_control = Control::Steps;
                    }
                    Outcome::Consumed
                }
                _ => Outcome::Consumed,
            },
            MouseEventKind::ScrollUp => self.move_selection(-1, ctx),
            MouseEventKind::ScrollDown => self.move_selection(1, ctx),
            _ => Outcome::Consumed,
        }
    }
}

fn contains(area: Rect, point: (u16, u16)) -> bool {
    point.0 >= area.x && point.0 < area.right() && point.1 >= area.y && point.1 < area.bottom()
}

impl Component for EnrichmentDialog {
    type Hit = EnrichmentHit;
    type Open = ();

    fn open(&mut self, _params: (), ctx: &mut Ctx<'_>) {
        self.open = true;
        self.geometry = EnrichmentGeometry::default();
        if let Some(state) = ctx.views.active_mut() {
            // An empty draft belongs to no step; keeping the id would resume it
            // as an edit of a stage the user is no longer editing.
            if state.enrichment.draft.is_empty() {
                state.enrichment_editing = None;
            }
            state.enrichment_control = Control::Steps;
        }
    }

    fn handle(&mut self, event: Event<EnrichmentHit>, ctx: &mut Ctx<'_>) -> Outcome {
        match event {
            Event::Key(key) => self.key(key, ctx),
            Event::Mouse { kind, hit, .. } => self.mouse(kind, hit, ctx),
            Event::Dismiss => {
                self.open = false;
                Outcome::Close
            }
            Event::Command(CommandId::EnrichmentAdd) => self.focus_and_activate(Control::Add, ctx),
            Event::Command(CommandId::EnrichmentEdit) => {
                self.focus_and_activate(Control::Edit, ctx)
            }
            Event::Command(CommandId::EnrichmentRemove) => {
                self.focus_and_activate(Control::Remove, ctx)
            }
            // §4.2: the list renders straight out of `ViewState`, so an
            // accepted or rejected chain needs no reaction beyond a redraw.
            Event::View(_) | Event::Command(_) | Event::Paste(_) | Event::Resize => {
                Outcome::Ignored
            }
        }
    }

    /// §4.3: the three step verbs are the palette's, contributed by the layer
    /// that owns them rather than gated by a peek at `Focus`.
    fn commands(&self, views: &Views) -> Vec<CommandEntry> {
        let stages = views.active().map_or(0, |state| state.enrichments.len());
        vec![
            CommandEntry {
                spec: CommandSpec {
                    id: CommandId::EnrichmentAdd,
                    name: "Add enrichment step",
                    description: "Open the step editor on a new derived field",
                    category: "Enrichment",
                    aliases: &["new step", "derive field"],
                    shortcut: self.open.then_some("Alt-a"),
                },
                unavailable_reason: (!self.open).then_some("open Enrichment first"),
            },
            CommandEntry {
                spec: CommandSpec {
                    id: CommandId::EnrichmentEdit,
                    name: "Edit enrichment step",
                    description: "Open the step editor on the selected step",
                    category: "Enrichment",
                    aliases: &["change step"],
                    shortcut: self.open.then_some("Alt-e"),
                },
                unavailable_reason: if !self.open {
                    Some("open Enrichment first")
                } else if stages == 0 {
                    Some("no steps yet")
                } else {
                    None
                },
            },
            CommandEntry {
                spec: CommandSpec {
                    id: CommandId::EnrichmentRemove,
                    name: "Remove enrichment step",
                    description: "Drop the selected step from the accepted chain",
                    category: "Enrichment",
                    aliases: &["delete step"],
                    shortcut: self.open.then_some("Alt-r"),
                },
                unavailable_reason: if !self.open {
                    Some("open Enrichment first")
                } else if stages == 0 {
                    Some("no steps yet")
                } else {
                    None
                },
            },
        ]
    }

    fn surface(&self) -> Surface {
        self.surface
    }

    fn hit(&self, point: (u16, u16)) -> Option<EnrichmentHit> {
        self.geometry
            .controls
            .iter()
            .find_map(|(rect, control)| {
                contains(*rect, point).then_some(EnrichmentHit::Control(*control))
            })
            .or_else(|| {
                self.geometry.rows.iter().find_map(|(rect, index)| {
                    contains(*rect, point).then_some(EnrichmentHit::Row(*index))
                })
            })
            .or_else(|| contains(self.surface.popup, point).then_some(EnrichmentHit::Body))
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx<'_>) -> Surface {
        render_enrichment_list(self, frame, area, ctx)
    }
}

/// Which way an enrichment chain change is being applied. Re-exported from
/// `app` so the two enrichment layers name the same fence the shell does.
pub(crate) use crate::app::PendingEnrichmentMutation as EnrichmentMutation;
/// Moved verbatim from `ui::render_enrichment_step_list`. The hit regions it
/// wrote into `App` are the component's geometry now; `view_state`,
/// `appearance.ascii` and the caret come from `ctx`. A free function taking
/// `&mut EnrichmentDialog` rather than a method, so the move stays line-for-line
/// comparable.
fn render_enrichment_list(
    this: &mut EnrichmentDialog,
    frame: &mut Frame<'_>,
    area: Rect,
    ctx: &RenderCtx<'_>,
) -> Surface {
    use crate::app::EnrichmentControl as Control;

    let theme = ctx.theme;
    let styles = DialogStyles::new(theme);
    let mut geometry = EnrichmentGeometry::default();
    let Some(state) = ctx.views.active() else {
        return this.record(geometry, Surface::default());
    };
    let stages = state.enrichments.clone();
    let selected = state
        .enrichment_selected
        .min(stages.len().saturating_sub(1));
    let focused = state.enrichment_control;
    let editor = state.enrichment.clone();
    let command = state.command_enrichment.clone();

    let labels = ["Add", "Edit", "Remove", "External command…"];
    let controls = [
        Control::Add,
        Control::Edit,
        Control::Remove,
        Control::ExternalCommand,
    ];
    let (message_state, mut sentence) = if let Some(error) = &editor.error {
        (
            MessageState::Error,
            format!("{error} · every accepted step is retained"),
        )
    } else if editor.pending_generation.is_some() {
        (
            MessageState::Updating,
            "checking a step · the accepted chain stays active".to_owned(),
        )
    } else if stages.is_empty() {
        (
            MessageState::Ready,
            "no steps yet · Add creates one".to_owned(),
        )
    } else {
        (
            MessageState::Applied,
            format!("{} steps active", stages.len()),
        )
    };
    if !editor.draft.trim().is_empty() {
        sentence.push_str(" · unsaved draft kept");
    }

    // §5.2: measure the natural body before choosing the popup height.
    let probe_width = class_l_width(area).saturating_sub(4).max(1);
    let action_rows = packed_button_rows(probe_width, &labels);
    let message_rows = message_rows(&sentence, probe_width);
    let steps_rows = 1 + stages.len().clamp(1, 8) as u16;
    let command_text = command.as_ref().map_or_else(
        || "Not configured".to_owned(),
        |stage| match &stage.definition.program {
            lvu_core::CommandProgram::Exec { executable, args } => {
                format!("{} · {} argument(s)", executable.display(), args.len())
            }
            lvu_core::CommandProgram::Shell { .. } => "Invalid saved command form".to_owned(),
        },
    );
    let help = "Later steps can use fields from earlier steps · the external command runs after all of them";
    let help_rows = Paragraph::new(help)
        .wrap(Wrap { trim: true })
        .line_count(probe_width)
        .clamp(1, 2) as u16;
    let natural_body = steps_rows + 1 + 2;
    let popup = class_l_popup(area, natural_body, message_rows, help_rows, action_rows);
    if popup.width < 20 || popup.height < 5 {
        return this.record(geometry, Surface::default());
    }
    clear_themed(frame, popup, theme);
    // §10: under a child this layer keeps its frame and title and drops to
    // `border` colour. `ctx.active` is what the legacy `active` argument was;
    // the shell knows which layer is on top and the parent decides what that
    // leaves it drawable (§6.5).
    let active = ctx.active;
    render_dialog_frame(frame, popup, " Enrichment ".to_owned(), active, theme);
    let regions = dialog_regions(popup, message_rows, help_rows, action_rows);
    let surface = Surface {
        popup,
        interior: regions.interior,
        ..Surface::default()
    };
    if !active {
        return this.record(geometry, surface);
    }

    // Body: the steps list, then the external-command summary.
    let body = regions.body;
    if body.height > 0 {
        // §5.4: the external-command pane keeps its heading and its one row
        // before the steps list is allowed to grow, and the gap goes first.
        let summary_rows = 2u16.min(body.height);
        let gap = u16::from(body.height > steps_rows.saturating_add(summary_rows));
        let list_height = body
            .height
            .saturating_sub(summary_rows.saturating_add(gap))
            .min(steps_rows)
            .max(1);
        let list_area = Rect::new(body.x, body.y, body.width, list_height);
        let count = (!stages.is_empty()).then(|| format!("{} of {}", selected + 1, stages.len()));
        let pane =
            render_pane_heading(frame, list_area, "Steps", count, stages.len().max(1), theme);
        let visible = usize::from(pane.viewport.height).max(1);
        let top = selected.saturating_sub(visible.saturating_sub(1));
        let mut rows = Vec::new();
        if stages.is_empty() {
            rows.push(Line::styled(
                "No steps yet · Add creates one",
                styles.description,
            ));
        } else {
            for (position, (index, stage)) in stages
                .iter()
                .enumerate()
                .skip(top)
                .take(visible)
                .enumerate()
            {
                let hit = Rect::new(
                    list_area.x,
                    pane.viewport.y + position as u16,
                    list_area.width,
                    1,
                );
                geometry.rows.push((hit, index));
                let marker = if index == selected {
                    if ctx.ascii { "> " } else { "› " }
                } else {
                    "  "
                };
                let prefix = format!("{marker}{}  ", index + 1);
                rows.push(Line::styled(
                    format!(
                        "{prefix}{}",
                        step_summary(
                            &stage.source,
                            usize::from(pane.viewport.width)
                                .saturating_sub(UnicodeWidthStr::width(prefix.as_str())),
                        )
                    ),
                    if index == selected && focused == Control::Steps {
                        styles.selection
                    } else if index == selected {
                        styles.label.add_modifier(Modifier::BOLD)
                    } else {
                        styles.label
                    },
                ));
            }
        }
        frame.render_widget(Paragraph::new(rows), pane.viewport);
        if let Some(bar) = pane.scrollbar {
            render_scrollbar(
                frame,
                bar,
                top,
                stages.len().saturating_sub(visible),
                theme,
                ctx.ascii,
            );
        }

        let summary_y = list_area.bottom().saturating_add(gap);
        if summary_y < body.bottom() {
            let summary_area = Rect::new(
                body.x,
                summary_y,
                body.width,
                body.bottom().saturating_sub(summary_y),
            );
            let pane = render_pane_heading(frame, summary_area, "External command", None, 1, theme);
            frame.render_widget(
                Paragraph::new(command_text)
                    .wrap(Wrap { trim: true })
                    .style(styles.description),
                pane.viewport,
            );
        }
    }

    // Message, help and actions.
    render_message(
        frame,
        regions.message,
        message_state,
        &sentence,
        theme,
        ctx.ascii,
    );
    if regions.help.height > 0 {
        frame.render_widget(
            Paragraph::new(help)
                .wrap(Wrap { trim: true })
                .style(styles.description),
            regions.help,
        );
    }
    if regions.actions.height > 0 {
        let focused_index = controls.iter().position(|control| *control == focused);
        for (index, hit) in button_layout(regions.actions, &labels, focused_index) {
            geometry.controls.push((hit, controls[index]));
            render_enrichment_button(
                frame,
                hit,
                labels[index],
                controls[index] == focused,
                index == 0,
                theme,
            );
        }
    }
    let _ = regions.content;
    this.record(geometry, surface)
}

/// Removing the selected step. Moved verbatim from
/// `App::remove_selected_enrichment`; the chain change is validated like
/// any other, and a rejection keeps every accepted step.
pub(crate) fn remove_selected_stage(ctx: &mut Ctx<'_>) {
    let Some(view_id) = ctx.views.active_id().map(str::to_owned) else {
        return;
    };
    let Some(state) = ctx.views.state(&view_id) else {
        return;
    };
    if state.enrichments.is_empty() {
        return;
    }
    let pending_draft = state.enrichment.draft.clone();
    let mut stages = state.enrichments.clone();
    let removed = stages
        .remove(state.enrichment_selected.min(stages.len() - 1))
        .id;
    if ctx
        .views
        .enqueue_enrichment_chain(&view_id, stages, pending_draft, EnrichmentMutation::Remove)
        .is_some()
    {
        let state = ctx.views.state_mut(&view_id).expect("view state");
        state.enrichment_selected = state
            .enrichment_selected
            .min(state.enrichments.len().saturating_sub(1));
        // An unfinished edit of the removed stage goes with it: it belongs
        // to no stage any more, and must never be resumed as a new step.
        if state.enrichment_editing.as_ref() == Some(&removed) {
            state.enrichment_editing = None;
            state.enrichment.draft.clear();
            ctx.cursors.reset(
                TextTarget {
                    identity: view_id,
                    field: "enrichment",
                },
                "",
            );
        }
    }
}
