//! The Fields layer (`docs/dialog-system.md` §12.11), converted per
//! `docs/component-model.md` §6.3 step 3.
//!
//! Fields reads the provider and writes the active view: it pins columns,
//! colours rows by a field, and hands the record off to Raw context or to the
//! cross-source correlation lookup. Which field is selected, which control has
//! focus and which record is anchored stay in `ViewState` — §7.3 records those
//! as accepted debt, because they persist per view across open and close. The
//! list's scroll offset does not: it is geometry, so it moved here, and `open`
//! zeroes it exactly as `Action::OpenFieldPicker` did.

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::{Frame, layout::Rect, style::Modifier, widgets::Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::app::{Action, FieldPickerControl, Views};
use crate::command_palette::CommandId;
use crate::component::{
    CommandEntry, CommandSpec, Component, Ctx, Event, Outcome, RenderCtx, Surface,
};
use crate::dialog_controls::DialogStyles;
use crate::provider::{DisplayRow, RowId, RowProvider};
use crate::ui::{
    FIELD_GUTTER, MessageState, dialog_frame_regions, help_rows, message_rows, packed_button_rows,
    render_action_row, render_help_text, render_message, render_scrollbar, truncated,
};

/// §12.11: the name column, wide enough for the field names a record carries
/// without pushing the value off the row.
const FIELD_NAME_WIDTH: u16 = 14;
/// `[ ] ` — the §8.4 checkbox and its trailing space.
const FIELD_CHECKBOX_WIDTH: u16 = 4;

/// Everything the Fields layer draws that can be clicked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FieldsHit {
    Row(usize),
    Control(FieldPickerControl),
}

/// Recorded by `render`, consumed by `hit()` (§5.1).
#[derive(Clone, Debug, Default)]
struct FieldsGeometry {
    rows: Vec<(Rect, usize)>,
    controls: Vec<(Rect, FieldPickerControl)>,
}

#[derive(Debug, Default)]
pub struct FieldsDialog {
    open: bool,
    /// First visible row of the field list.
    top: usize,
    geometry: FieldsGeometry,
    surface: Surface,
}

/// The record the dialog is showing, read fresh every frame: a component never
/// caches rows (§7.10).
pub fn anchored_row(views: &Views, provider: &dyn RowProvider) -> Option<DisplayRow> {
    let id = anchor_id(views)?.clone();
    let view_id = views.active_id()?;
    provider.row_by_id(view_id, &id)
}

pub fn anchor_id(views: &Views) -> Option<&RowId> {
    views.active()?.field_picker_row.as_ref()
}

/// Scrolls only far enough to keep `selected` on screen, as the shared
/// viewport helper did.
fn reveal(top: usize, selected: usize, visible: usize) -> usize {
    if selected < top {
        selected
    } else if visible > 0 && selected >= top.saturating_add(visible) {
        selected.saturating_add(1).saturating_sub(visible)
    } else {
        top
    }
}

fn contains(area: Rect, point: (u16, u16)) -> bool {
    point.0 >= area.x && point.0 < area.right() && point.1 >= area.y && point.1 < area.bottom()
}

/// §4.3: the palette entries Fields owns.
const FIELDS_COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        id: CommandId::PinField,
        name: "Pin or unpin field",
        description: "Toggle the selected field",
        category: "Fields",
        aliases: &["column", "visible"],
        shortcut: None,
    },
    CommandSpec {
        id: CommandId::ColorField,
        name: "Color rows by this field",
        description: "Toggle color rules for the selected field",
        category: "Fields",
        aliases: &["highlight", "style"],
        shortcut: None,
    },
    CommandSpec {
        id: CommandId::CorrelateField,
        name: "Correlate across sources",
        description: "Find records with the selected field value in open sources",
        category: "Fields",
        aliases: &["matching records", "same value", "related logs"],
        shortcut: None,
    },
];

/// The key that reaches each command while the layer is on top.
fn fields_command_shortcut(id: CommandId) -> Option<&'static str> {
    match id {
        CommandId::PinField => Some("Space"),
        CommandId::ColorField => Some("Alt-C"),
        CommandId::CorrelateField => Some("Alt-R"),
        _ => None,
    }
}

impl FieldsDialog {
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Marks the layer open without a `Ctx`, so a palette test can ask for the
    /// entries it contributes while it is on the stack.
    pub fn open_for_test(&mut self) {
        self.open = true;
    }

    pub fn top(&self) -> usize {
        self.top
    }

    /// Geometry recorded by the last `render`; `hit()` is how input reaches it.
    pub fn row_rects(&self) -> &[(Rect, usize)] {
        &self.geometry.rows
    }

    pub fn control_rects(&self) -> &[(Rect, FieldPickerControl)] {
        &self.geometry.controls
    }

    fn record(
        &mut self,
        rows: Vec<(Rect, usize)>,
        controls: Vec<(Rect, FieldPickerControl)>,
        surface: Surface,
    ) -> Surface {
        self.geometry = FieldsGeometry { rows, controls };
        self.surface = surface;
        surface
    }

    fn control(ctx: &Ctx<'_>) -> FieldPickerControl {
        ctx.views
            .active()
            .map_or(FieldPickerControl::List, |state| state.field_picker_control)
    }

    /// The field name the actions would act on, resolved through the provider
    /// every time rather than remembered (§7.10).
    fn selected_field(ctx: &Ctx<'_>) -> Option<String> {
        let row = anchored_row(ctx.views, ctx.provider)?;
        let selected = ctx
            .views
            .active()
            .map_or(0, |state| state.field_picker_selected);
        row.fields.get(selected).map(|(field, _)| field.clone())
    }

    fn move_selection(&mut self, delta: i32, ctx: &mut Ctx<'_>) {
        if ctx.correlating {
            return;
        }
        let count = anchored_row(ctx.views, ctx.provider).map_or(0, |row| row.fields.len());
        if let Some(state) = ctx.views.active_mut()
            && count > 0
        {
            state.field_picker_selected =
                (state.field_picker_selected as i32 + delta).rem_euclid(count as i32) as usize;
        }
    }

    fn move_control(&mut self, delta: i32, ctx: &mut Ctx<'_>) {
        let controls = [
            FieldPickerControl::List,
            FieldPickerControl::Pin,
            FieldPickerControl::Color,
            FieldPickerControl::Correlate,
            FieldPickerControl::Context,
        ];
        if let Some(state) = ctx.views.active_mut() {
            let at = controls
                .iter()
                .position(|control| *control == state.field_picker_control)
                .unwrap_or(0);
            state.field_picker_control = controls
                [(at as isize + delta as isize).rem_euclid(controls.len() as isize) as usize];
        }
    }

    fn focus_control(&mut self, control: FieldPickerControl, ctx: &mut Ctx<'_>) {
        if let Some(state) = ctx.views.active_mut() {
            state.field_picker_control = control;
        }
    }

    fn activate(&mut self, ctx: &mut Ctx<'_>) -> Outcome {
        match Self::control(ctx) {
            FieldPickerControl::Context => self.open_context(ctx),
            FieldPickerControl::Correlate => self.correlate(ctx),
            FieldPickerControl::Color => self.toggle_field(false, ctx),
            // The list's own activation is the pin, so Enter on a row does
            // what Space does.
            FieldPickerControl::List | FieldPickerControl::Pin => self.toggle_field(true, ctx),
        }
    }

    /// Pin/unpin, or colour/stop colouring, the selected field. Both write the
    /// active view and nothing else, so the log re-renders from `Views` (§4.2).
    fn toggle_field(&mut self, pin: bool, ctx: &mut Ctx<'_>) -> Outcome {
        if ctx.correlating {
            return Outcome::Consumed;
        }
        let Some(field) = Self::selected_field(ctx) else {
            return Outcome::Consumed;
        };
        let Some(state) = ctx.views.active_mut() else {
            return Outcome::Consumed;
        };
        if pin {
            if let Some(index) = state
                .pinned_columns
                .iter()
                .position(|value| value == &field)
            {
                state.pinned_columns.remove(index);
            } else if state.pinned_columns.len() < 8 {
                state.pinned_columns.push(field);
            }
        } else if state.color_field.as_deref() == Some(&field) {
            state.color_field = None;
        } else {
            state.color_field = Some(field);
        }
        state.user_interaction_revision = state.user_interaction_revision.saturating_add(1);
        Outcome::Consumed
    }

    /// The correlation queue is the shell's until Correlation converts (§6.3),
    /// so the layer hands it the record and field and stays put.
    fn correlate(&mut self, ctx: &mut Ctx<'_>) -> Outcome {
        if ctx.correlating {
            return Outcome::Consumed;
        }
        let Some(row) = anchored_row(ctx.views, ctx.provider) else {
            ctx.notice("field data is pending or unavailable");
            return Outcome::Consumed;
        };
        let Some(field) = Self::selected_field(ctx) else {
            ctx.notice("field data is pending or unavailable");
            return Outcome::Consumed;
        };
        Outcome::Defer(Action::CorrelateField { row: row.id, field })
    }

    /// Raw context converts next (§6.3 step 4); until then it is a legacy
    /// dialog that returns to this layer, so Fields stays on the stack.
    fn open_context(&mut self, ctx: &mut Ctx<'_>) -> Outcome {
        let Some(anchor) = anchor_id(ctx.views).cloned() else {
            return Outcome::Consumed;
        };
        Outcome::Defer(Action::OpenContextForLayer(anchor))
    }

    fn key(&mut self, key: KeyEvent, ctx: &mut Ctx<'_>) -> Outcome {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return Outcome::Ignored;
        }
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1, ctx),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1, ctx),
            KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.move_control(-1, ctx)
            }
            KeyCode::BackTab => self.move_control(-1, ctx),
            KeyCode::Tab => self.move_control(1, ctx),
            // Space always pins, wherever focus sits (§8.4); Enter activates
            // whichever control has it.
            KeyCode::Char(' ') => return self.toggle_field(true, ctx),
            KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::ALT) => {
                return self.toggle_field(true, ctx);
            }
            KeyCode::Enter => return self.activate(ctx),
            KeyCode::Char('c') => return self.toggle_field(false, ctx),
            KeyCode::Char('o') => return self.open_context(ctx),
            KeyCode::Char('r') => return self.correlate(ctx),
            _ => return Outcome::Ignored,
        }
        Outcome::Consumed
    }

    fn mouse(
        &mut self,
        kind: MouseEventKind,
        hit: Option<FieldsHit>,
        ctx: &mut Ctx<'_>,
    ) -> Outcome {
        // A pending correlation freezes the dialog: no pin, no colour and no
        // second lookup until this one settles or is cancelled.
        if !ctx.correlating && matches!(kind, MouseEventKind::Down(MouseButton::Left)) {
            match hit {
                Some(FieldsHit::Row(index)) => {
                    if let Some(state) = ctx.views.active_mut() {
                        state.field_picker_selected = index;
                        state.field_picker_control = FieldPickerControl::List;
                    }
                }
                Some(FieldsHit::Control(control)) => {
                    self.focus_control(control, ctx);
                    return self.activate(ctx);
                }
                None => {}
            }
        }
        match kind {
            MouseEventKind::ScrollUp => self.move_selection(-1, ctx),
            MouseEventKind::ScrollDown => self.move_selection(1, ctx),
            _ => {}
        }
        Outcome::Consumed
    }
}

impl Component for FieldsDialog {
    type Hit = FieldsHit;
    type Open = ();

    fn open(&mut self, _params: (), ctx: &mut Ctx<'_>) {
        self.open = true;
        self.top = 0;
        self.geometry = FieldsGeometry::default();
        if let Some(state) = ctx.views.active_mut() {
            // Freeze the identity, not the current row projection. A cache miss
            // is pending work and must not prevent the dialog opening.
            state.field_picker_row = state.selected.clone();
            state.field_picker_selected = 0;
        }
    }

    fn handle(&mut self, event: Event<FieldsHit>, ctx: &mut Ctx<'_>) -> Outcome {
        match event {
            Event::Key(key) => self.key(key, ctx),
            Event::Mouse { kind, hit, .. } => self.mouse(kind, hit, ctx),
            // Escape abandons the lookup as well as the dialog; the queue is
            // the shell's, so it is told after the layer is gone.
            Event::Dismiss => {
                self.open = false;
                Outcome::Legacy(Action::CancelCorrelation)
            }
            Event::Command(CommandId::PinField) => self.toggle_field(true, ctx),
            Event::Command(CommandId::ColorField) => self.toggle_field(false, ctx),
            Event::Command(CommandId::CorrelateField) => self.correlate(ctx),
            Event::Command(_) | Event::Paste(_) | Event::View(_) | Event::Resize => {
                Outcome::Ignored
            }
        }
    }

    fn commands(&self, _views: &Views) -> Vec<CommandEntry> {
        FIELDS_COMMANDS
            .iter()
            .map(|spec| CommandEntry {
                spec: CommandSpec {
                    shortcut: self
                        .open
                        .then(|| fields_command_shortcut(spec.id))
                        .flatten(),
                    ..*spec
                },
                unavailable_reason: (!self.open).then_some("open Fields first"),
            })
            .collect()
    }

    fn surface(&self) -> Surface {
        self.surface
    }

    fn hit(&self, point: (u16, u16)) -> Option<FieldsHit> {
        self.geometry
            .controls
            .iter()
            .find_map(|(rect, control)| {
                contains(*rect, point).then_some(FieldsHit::Control(*control))
            })
            .or_else(|| {
                self.geometry.rows.iter().find_map(|(rect, index)| {
                    contains(*rect, point).then_some(FieldsHit::Row(*index))
                })
            })
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx<'_>) -> Surface {
        use crate::dialog_layout::{DialogClass, DialogContent, content_width, pane};
        use FieldPickerControl as C;
        let theme = ctx.theme;
        let styles = DialogStyles::new(theme);
        let ascii = ctx.ascii;
        let mut rows_hit: Vec<(Rect, usize)> = Vec::new();
        let mut controls_hit: Vec<(Rect, FieldPickerControl)> = Vec::new();
        let width = content_width(area, DialogClass::M);
        let row = anchored_row(ctx.views, ctx.provider);
        let has_anchor = anchor_id(ctx.views).is_some();
        let title = match anchor_id(ctx.views) {
            Some(id) => format!("Fields · record {}", id.sequence),
            None => "Fields".to_owned(),
        };
        let fields = row.as_ref().map_or_else(Vec::new, |row| row.fields.clone());
        // While the correlation lookup runs the dialog is read-only: no pin, no
        // colour and no second lookup. It says so rather than looking idle.
        let pending = ctx.correlating;
        let (state_word, sentence) = if pending {
            (
                MessageState::Pending,
                "finding records that share this value".to_owned(),
            )
        } else if row.is_none() && has_anchor {
            (
                MessageState::Pending,
                "field data for this record has not arrived yet".to_owned(),
            )
        } else if row.is_none() {
            (
                MessageState::Disabled,
                "select a record to see its fields".to_owned(),
            )
        } else {
            (MessageState::Ready, String::new())
        };
        // §12.11: no message row when there is no state to report.
        let quiet = sentence.is_empty();
        let help = if pending {
            "The view you are in does not change while the lookup runs."
        } else if fields.is_empty() {
            ""
        } else {
            "Pinned fields become log columns."
        };

        let control = ctx
            .views
            .active()
            .map_or(C::List, |state| state.field_picker_control);
        let selected = ctx
            .views
            .active()
            .map_or(0, |state| state.field_picker_selected);
        let pinned = ctx
            .views
            .active()
            .map_or_else(Vec::new, |state| state.pinned_columns.clone());
        let color_field = ctx
            .views
            .active()
            .and_then(|state| state.color_field.clone());
        let selected_key = fields.get(selected).map(|(key, _)| key.clone());
        let pin_label = if selected_key
            .as_ref()
            .is_some_and(|key| pinned.contains(key))
        {
            "&Unpin"
        } else {
            "&Pin"
        };
        let color_label = if selected_key
            .as_deref()
            .is_some_and(|key| color_field.as_deref() == Some(key))
        {
            "Stop &colouring by field"
        } else {
            "&Color rows by field"
        };
        let actions: Vec<(&str, C)> = if pending {
            Vec::new()
        } else if !fields.is_empty() {
            vec![
                (pin_label, C::Pin),
                (color_label, C::Color),
                ("Co&rrelate across sources", C::Correlate),
            ]
        } else if has_anchor {
            // Nothing to pin, but the record itself is still inspectable.
            vec![("Raw c&ontext", C::Context)]
        } else {
            Vec::new()
        };
        let action_labels = actions.iter().map(|(label, _)| *label).collect::<Vec<_>>();

        let list_rows = fields.len().clamp(1, 16);
        let content = DialogContent {
            header: 0,
            body: u16::try_from(list_rows + 1).unwrap_or(u16::MAX),
            message: if quiet {
                0
            } else {
                message_rows(&sentence, width)
            },
            help: help_rows(help, width),
            actions: packed_button_rows(width, &action_labels),
        };
        let regions = dialog_frame_regions(frame, area, DialogClass::M, &title, &content, theme);
        let surface = Surface {
            popup: regions.popup,
            interior: regions.interior,
            caret: None,
            scrollable: true,
            // Fields takes no text, so `q` dismisses it (§1).
            text_focus: false,
        };
        let inner = regions.body;
        if inner.width == 0 || inner.height == 0 {
            return self.record(rows_hit, controls_hit, surface);
        }

        let count = format!(
            "{} field{}",
            fields.len(),
            if fields.len() == 1 { "" } else { "s" }
        );
        let rects = pane(
            inner,
            u16::try_from(UnicodeWidthStr::width(count.as_str())).unwrap_or(0),
            fields.len(),
        );
        if rects.heading.height > 0 {
            // §4.4: the heading names the two columns the rows line up under.
            frame.render_widget(
                Paragraph::new("Field").style(styles.label.add_modifier(Modifier::BOLD)),
                rects.heading,
            );
            let value_x = rects
                .heading
                .x
                .saturating_add(crate::dialog_layout::PANE_INDENT)
                // The rows lead with the selection marker as well as the checkbox.
                .saturating_add(FIELD_CHECKBOX_WIDTH + 2)
                .saturating_add(FIELD_NAME_WIDTH)
                .saturating_add(FIELD_GUTTER);
            if value_x < rects.count.x.max(rects.heading.right()) {
                frame.render_widget(
                    Paragraph::new("Value").style(styles.label.add_modifier(Modifier::BOLD)),
                    Rect::new(
                        value_x,
                        rects.heading.y,
                        rects.heading.right().saturating_sub(value_x),
                        1,
                    ),
                );
            }
            if rects.count.width > 0 {
                frame.render_widget(Paragraph::new(count).style(styles.description), rects.count);
            }
        }

        let visible = usize::from(rects.viewport.height);
        // Keeping the selection inside the list is geometry, so it is decided here
        // and written back to the view that owns the offset (§5.1).
        // Keeping the selection inside the list is geometry, so the offset is the
        // component's (§5.1/§7.3); `open` resets it exactly as before.
        self.top = reveal(self.top, selected, visible);
        let top = self.top;
        if fields.is_empty() {
            if rects.viewport.height > 0 {
                frame.render_widget(
                    Paragraph::new(truncated(
                        match (row.is_some(), has_anchor) {
                            (true, _) => "No fields for this record",
                            // The message row is already saying the record has not
                            // arrived; repeating it here as a false negative would
                            // read as "this record has no fields".
                            (false, true) => "",
                            (false, false) => "No record selected",
                        },
                        usize::from(rects.viewport.width),
                    ))
                    .style(styles.unavailable),
                    Rect::new(rects.viewport.x, rects.viewport.y, rects.viewport.width, 1),
                );
            }
        } else {
            for (offset, (index, (key, value))) in fields
                .iter()
                .enumerate()
                .skip(top)
                .take(visible)
                .enumerate()
            {
                let y = rects.viewport.y.saturating_add(offset as u16);
                let row_rect = Rect::new(rects.viewport.x, y, rects.viewport.width, 1);
                let focused = index == selected;
                let style = if focused && control == C::List {
                    styles.selection
                } else if focused {
                    styles.label
                } else {
                    styles.description
                };
                // §8.4: the checkbox says whether the field is pinned; the marker
                // says which row the actions would act on.
                let marker = if focused {
                    if ascii { "> " } else { "› " }
                } else {
                    "  "
                };
                let box_text = if pinned.contains(key) { "[x]" } else { "[ ]" };
                let lead = format!("{marker}{box_text} ");
                let lead_width = (FIELD_CHECKBOX_WIDTH + 2).min(row_rect.width);
                frame.render_widget(
                    Paragraph::new(truncated(&lead, usize::from(lead_width))).style(style),
                    Rect::new(row_rect.x, y, lead_width, 1),
                );
                let name_x = row_rect.x.saturating_add(lead_width);
                let name_width = FIELD_NAME_WIDTH.min(row_rect.right().saturating_sub(name_x));
                frame.render_widget(
                    Paragraph::new(truncated(key, usize::from(name_width))).style(style),
                    Rect::new(name_x, y, name_width, 1),
                );
                let value_x = name_x
                    .saturating_add(name_width)
                    .saturating_add(FIELD_GUTTER);
                if value_x < row_rect.right() {
                    let value_width = row_rect.right().saturating_sub(value_x);
                    let shown = if color_field.as_deref() == Some(key.as_str()) {
                        format!("{value} · colouring rows")
                    } else {
                        value.clone()
                    };
                    frame.render_widget(
                        Paragraph::new(truncated(&shown, usize::from(value_width))).style(style),
                        Rect::new(value_x, y, value_width, 1),
                    );
                }
                // Inert while a lookup runs, so click and paint cannot disagree.
                if !pending {
                    rows_hit.push((row_rect, index));
                }
            }
        }
        if let Some(bar) = rects.scrollbar {
            render_scrollbar(
                frame,
                bar,
                top,
                fields.len().saturating_sub(visible),
                theme,
                ascii,
            );
        }
        if !quiet {
            render_message(frame, regions.message, state_word, &sentence, theme, ascii);
        }
        render_help_text(frame, regions.help, help, theme);
        let focused = actions
            .iter()
            .position(|(_, candidate)| *candidate == control);
        for (index, rect) in
            render_action_row(frame, regions.actions, &action_labels, focused, &[], theme)
        {
            controls_hit.push((rect, actions[index].1));
        }
        self.record(rows_hit, controls_hit, surface)
    }
}
