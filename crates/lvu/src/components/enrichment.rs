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
//!
//! Command steps are rows of the same list (§12.5): `Edit` on one opens the
//! External command dialog on it, `Remove` drops it through the same chain
//! mutation, and `External command…` inserts a new one after the selection.

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::{Frame, layout::Rect, style::Modifier, text::Line, widgets::Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::app::{EnrichmentControl as Control, Views};
use crate::command_palette::CommandId;
use crate::component::{
    CommandEntry, CommandSpec, Component, Ctx, Event, Open, Outcome, RenderCtx, Surface,
};
use crate::dialog_controls::{ButtonRole, DialogStyles, render_role_button, stable_action_rows};
use crate::dialog_layout::{DialogSpec, PresentationKind, plan_list, resolve_dialog};
use crate::text_edit::TextTarget;
use crate::ui::{
    MessageState, render_help_text, render_message, render_responsive_frame, render_scrollbar,
    step_summary,
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
            Some(stage) if stage.is_command() => Outcome::Replace(Open::ExternalCommand {
                stage: Some(stage.id),
                insert_at: 0,
            }),
            Some(stage) => Outcome::OpenChild(Open::EnrichmentStep {
                prefill: None,
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

    /// `External command…` opens the selected command step, or inserts a
    /// new one after the selection (at the end of an empty chain). `Edit`
    /// on a command row opens it too.
    fn external_command(&mut self, ctx: &mut Ctx<'_>) -> Outcome {
        let Some(state) = ctx.views.active() else {
            return Outcome::Consumed;
        };
        let selected = state.enrichments.get(
            state
                .enrichment_selected
                .min(state.enrichments.len().saturating_sub(1)),
        );
        match selected {
            Some(stage) if stage.is_command() => Outcome::Replace(Open::ExternalCommand {
                stage: Some(stage.id.clone()),
                insert_at: 0,
            }),
            Some(_) => Outcome::Replace(Open::ExternalCommand {
                stage: None,
                insert_at: state.enrichment_selected.saturating_add(1),
            }),
            None => Outcome::Replace(Open::ExternalCommand {
                stage: None,
                insert_at: 0,
            }),
        }
    }

    /// Alt-Up / Alt-Down move the selected step. The reordered chain is
    /// validated like any other change: a step moved above one it reads is
    /// rejected and every accepted step stays.
    fn move_step(&mut self, delta: i32, ctx: &mut Ctx<'_>) -> Outcome {
        let Some(view_id) = ctx.views.active_id().map(str::to_owned) else {
            return Outcome::Consumed;
        };
        let Some(state) = ctx.views.state(&view_id) else {
            return Outcome::Consumed;
        };
        let len = state.enrichments.len();
        if len < 2 {
            return Outcome::Consumed;
        }
        let from = state.enrichment_selected.min(len - 1);
        let to = from as i32 + delta;
        if to < 0 || to >= len as i32 {
            return Outcome::Consumed;
        }
        let to = to as usize;
        let mut chain = state.enrichments.clone();
        chain.swap(from, to);
        let pending_draft = state.enrichment.draft.clone();
        if ctx
            .views
            .enqueue_enrichment_chain(&view_id, chain, pending_draft, EnrichmentMutation::Reorder)
            .is_some()
            && let Some(state) = ctx.views.state_mut(&view_id)
        {
            state.enrichment_selected = to;
        }
        Outcome::Consumed
    }

    /// §8.9: the default action follows the list. With nothing to edit the
    /// only sensible verb is `Add`; once a step is selected it is `Edit`. The
    /// render fills whichever button this names, so the two stay one fact.
    pub fn default_control(views: &Views) -> Control {
        if views
            .active()
            .is_some_and(|state| !state.enrichments.is_empty())
        {
            Control::Edit
        } else {
            Control::Add
        }
    }

    /// Activating whatever the focus ring is on. `Steps` is the list itself,
    /// where Enter executes the default (§8.5): edit the selected step, or
    /// add the first one when there is nothing to select.
    fn activate(&mut self, control: Control, ctx: &mut Ctx<'_>) -> Outcome {
        match control {
            Control::Steps => {
                let default = Self::default_control(ctx.views);
                self.activate(default, ctx)
            }
            Control::Add => Outcome::OpenChild(Open::EnrichmentStep {
                editing: None,
                prefill: None,
            }),
            Control::Edit => self.edit_selected(ctx),
            Control::Remove => self.remove_selected(ctx),
            // §6.5: `Replace`, not `OpenChild`. See the module doc.
            Control::ExternalCommand => self.external_command(ctx),
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
        // The four accelerators are the §8.10 mnemonics of the four buttons and
        // the shell resolves them in `dispatch_raw`; they fire their verb
        // without moving the focus ring, and only a click both focuses and
        // fires.
        match key.code {
            KeyCode::Tab => self.move_control(
                if key.modifiers.contains(KeyModifiers::SHIFT) {
                    -1
                } else {
                    1
                },
                ctx,
            ),
            KeyCode::BackTab => self.move_control(-1, ctx),
            KeyCode::Up if alt => self.move_step(-1, ctx),
            KeyCode::Down if alt => self.move_step(1, ctx),
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

/// §8.9/§8.10: the action row, in drawn order. One declaration, read by
/// `render` for the underlines and by the shell for the keys behind them.
const ACTION_LABELS: [&str; 4] = ["&Add", "&Edit", "&Remove", "External &command…"];

const ACTION_CONTROLS: [Control; 4] = [
    Control::Add,
    Control::Edit,
    Control::Remove,
    Control::ExternalCommand,
];

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
            // §8.5: a list opens on a real row. The selection is view-owned
            // and outlives the chain it indexed, so clamp it here rather than
            // in every reader.
            state.enrichment_selected = state
                .enrichment_selected
                .min(state.enrichments.len().saturating_sub(1));
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
                    shortcut: self.open.then_some("a"),
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
                    shortcut: self.open.then_some("e"),
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
                    shortcut: self.open.then_some("r"),
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

    fn action_labels(&self, _ctx: &Ctx<'_>) -> Vec<&'static str> {
        ACTION_LABELS.to_vec()
    }

    fn press_action(&mut self, index: usize, ctx: &mut Ctx<'_>) -> Outcome {
        match ACTION_CONTROLS.get(index) {
            Some(control) => self.activate(*control, ctx),
            None => Outcome::Ignored,
        }
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
/// Stable LongContent budgets: outer size is policy-only, never the step count
/// or pending state, so empty/populated/pending/error frames share one `frame`
/// and sticky tail origins. The body owns the surplus via the shared list
/// pane. Hand-rolled row counts here are presentation-only folding (AGENTS.md).
fn enrichment_spec(area: Rect) -> DialogSpec {
    let (policy_w, _) = crate::dialog_layout::policy_size(area, PresentationKind::LongContent);
    let estimate = policy_w.saturating_sub(4).max(1);
    let actions = stable_action_rows(estimate, &ACTION_LABELS).clamp(1, 2);
    DialogSpec::new(PresentationKind::LongContent, 0, 3, 2, 2, actions)
}

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
    let stale = stale_command_steps(state);

    let labels = ACTION_LABELS;
    let controls = ACTION_CONTROLS;
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
    } else if !stale.is_empty() {
        (
            MessageState::Pending,
            format!(
                "{} steps active · {} unrun: {}",
                stages.len(),
                stale.len(),
                stale.join(", ")
            ),
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

    // §5.2: the frame is policy-only (stable across pending/empty/populated/
    // error states); the step count sizes only the scroll extent.
    let help = "Later steps can use fields from earlier steps, command output as <name>.<field> · Alt-Up/Down reorder · commands run only when you confirm";
    let active = ctx.active;
    let default = EnrichmentDialog::default_control(ctx.views);
    let default_index = controls.iter().position(|control| *control == default);
    let spec = enrichment_spec(area);
    let Ok(resolved) = resolve_dialog(
        area,
        &spec,
        stages.len().max(1),
        &labels,
        default_index,
        None,
    ) else {
        // Below the 20x6 floor the tiny fallback owns the frame; stay open
        // with nothing drawn, as the palette does.
        return this.record(geometry, Surface::default());
    };
    render_responsive_frame(frame, &resolved, "Enrichment", active, theme);
    let mut surface = Surface {
        popup: resolved.frontmost,
        interior: resolved.interior,
        ..Surface::default()
    };
    // §10: under a child this layer keeps its frame and title and drops to
    // `border` colour. `ctx.active` is what the legacy `active` argument was;
    // the shell knows which layer is on top and the parent decides what that
    // leaves it drawable (§6.5).
    if !active {
        return this.record(geometry, surface);
    }

    // Body: the steps list, expression and command steps in chain order. One
    // authoritative shared plan drives heading, count, viewport, scrollbar,
    // selection window, paint and mouse.
    let count_text = if stages.is_empty() {
        "0 of 0".to_owned()
    } else {
        format!("{} of {}", selected + 1, stages.len())
    };
    let count_width = u16::try_from(UnicodeWidthStr::width(count_text.as_str())).unwrap_or(0);
    let list = plan_list(
        resolved.body.viewport,
        count_width,
        stages.len(),
        (!stages.is_empty()).then_some(selected),
        0,
    );
    if list.heading.height > 0 {
        frame.render_widget(
            Paragraph::new("Steps").style(styles.label.add_modifier(Modifier::BOLD)),
            list.heading,
        );
        if list.count.width > 0 {
            frame.render_widget(
                Paragraph::new(Line::from(count_text)).style(styles.description),
                list.count,
            );
        }
    }
    if stages.is_empty() {
        if list.viewport.height > 0 {
            frame.render_widget(
                Paragraph::new("No steps yet · Add creates one").style(styles.description),
                Rect::new(list.viewport.x, list.viewport.y, list.viewport.width, 1),
            );
        }
    } else {
        for (offset, row) in list.row_rects.iter().enumerate() {
            let index = list.first_row.saturating_add(offset);
            let Some(stage) = stages.get(index) else {
                continue;
            };
            geometry.rows.push((*row, index));
            let marker = if index == selected {
                if ctx.ascii { "> " } else { "› " }
            } else {
                "  "
            };
            let prefix = format!("{marker}{}  ", index + 1);
            let width =
                usize::from(row.width).saturating_sub(UnicodeWidthStr::width(prefix.as_str()));
            let text = match &stage.command {
                Some(command) => command_row(stage, command, state, ctx.ascii, width),
                None => step_summary(&stage.source, width),
            };
            frame.render_widget(
                Paragraph::new(Line::styled(
                    format!("{prefix}{text}"),
                    if index == selected && focused == Control::Steps {
                        styles.selection
                    } else if index == selected {
                        styles.label.add_modifier(Modifier::BOLD)
                    } else {
                        styles.label
                    },
                )),
                *row,
            );
        }
    }
    if let Some(bar) = list.scrollbar {
        render_scrollbar(
            frame,
            bar,
            list.first_row,
            stages.len().saturating_sub(list.row_rects.len()),
            theme,
            ctx.ascii,
        );
    }
    surface.scrollable = list.scrollbar.is_some();

    // Message, help and actions: sticky tail bands from the shared geometry.
    render_message(
        frame,
        resolved.message,
        message_state,
        &sentence,
        theme,
        ctx.ascii,
    );
    render_help_text(frame, resolved.help, help, theme);
    {
        let focused_index = controls.iter().position(|control| *control == focused);
        for (index, hit) in &resolved.actions.buttons {
            geometry.controls.push((*hit, controls[*index]));
            render_role_button(
                frame,
                *hit,
                labels[*index],
                if resolved.actions.default == Some(*index) {
                    ButtonRole::Default
                } else {
                    ButtonRole::Normal
                },
                focused_index == Some(*index),
                theme,
            );
        }
    }
    this.record(geometry, surface)
}

/// The names of the command steps whose results are missing or older than
/// their definition (§7.4 `Unrun`): the chain is applied, these rows are not.
pub(crate) fn stale_command_steps(state: &crate::app::ViewState) -> Vec<String> {
    state
        .enrichments
        .iter()
        .filter(|stage| stage.is_command())
        .filter(|stage| {
            state
                .command_steps
                .get(&stage.id.0)
                .is_none_or(|run| run.publication.is_none())
        })
        .map(|stage| stage.source.clone())
        .collect()
}

/// One command row: the output name, the program, and the run state the
/// dialog would show for it, so the list is enough to know what is stale.
fn command_row(
    stage: &crate::app::EnrichmentDefinition,
    command: &lvu_core::CommandDefinition,
    state: &crate::app::ViewState,
    ascii: bool,
    width: usize,
) -> String {
    let glyph = if ascii { "$" } else { "⚙" };
    let program = match &command.program {
        lvu_core::CommandProgram::Exec { executable, args } => {
            let mut text = executable.display().to_string();
            for arg in args {
                text.push(' ');
                text.push_str(arg);
            }
            text
        }
        lvu_core::CommandProgram::Shell { .. } => "invalid saved command form".to_owned(),
    };
    let run_state = if state
        .command_steps
        .get(&stage.id.0)
        .is_some_and(|run| run.publication.is_some())
    {
        "results published"
    } else {
        "unrun"
    };
    // State before program: a long path truncates, the state never does.
    let text = format!("{glyph} {} · {run_state} · {program}", stage.source);
    step_summary(&text, width)
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
