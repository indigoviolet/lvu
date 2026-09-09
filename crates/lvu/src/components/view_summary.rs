//! The View summary layer (`docs/dialog-system.md` §12.22): one read-only
//! stack of everything applied to the current view, in the order the view
//! evaluates it, one row per operation.
//!
//! Every value here is *read* from `Views` on each frame and rendered the way
//! the dialog that owns it would render it; nothing is edited and nothing is
//! persisted. The layer's one verb is `Open`: Enter (or the button) pops this
//! layer and pushes the dialog that owns the selected row, with that row's
//! item selected — a `Replace`, not an `OpenChild`, because the summary is a
//! launcher rather than a parent the user returns to (component-model §6.5).
//!
//! The list has a fixed shape: a row for an operation that is not applied
//! reads `—` rather than disappearing, so the same row is always in the same
//! place and nothing here is a live region (§5.2.1).

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::{Frame, layout::Rect, style::Modifier, text::Line, widgets::Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::app::{
    CaptureTimePolicy, DEFAULT_FOLD_MINIMUM_RUN, SourceItem, TimeBasis, ViewRole, ViewState, Views,
    format_capture_duration, format_utc_nanos, move_control, time_basis_label,
};
use crate::component::{Component, Ctx, Event, Open, Outcome, RenderCtx, Surface};
use crate::components::enrichment::stale_command_steps;
use crate::components::folding::fold_key_label;
use crate::dialog_controls::{ActionRow, DialogStyles};
use crate::provider::ViewOrder;
use crate::ui::{
    MessageState, dialog_frame_regions, help_rows, message_rows, packed_button_rows,
    render_actions, render_help_text, render_message, render_scrollbar, truncated,
};

/// One row of the summary. The order of `ALL` is the order the rows are drawn
/// in, which is the order the view evaluates them: what it reads, the window
/// it reads within, the columns it derives, the predicates that keep rows, the
/// presentation of what is left, and finally whether all of that is current.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SummaryRow {
    /// Why the view exists: the source's own unfiltered view, or one derived
    /// from a source.
    Role,
    Sources,
    Time,
    Enrichment,
    Search,
    Filter,
    Grouping,
    Fold,
    Columns,
    Colour,
    /// Whether what the rows show is the applied definition, or is still
    /// catching up with it.
    Readiness,
}

impl SummaryRow {
    pub const ALL: [SummaryRow; 11] = [
        SummaryRow::Role,
        SummaryRow::Sources,
        SummaryRow::Time,
        SummaryRow::Enrichment,
        SummaryRow::Search,
        SummaryRow::Filter,
        SummaryRow::Grouping,
        SummaryRow::Fold,
        SummaryRow::Columns,
        SummaryRow::Colour,
        SummaryRow::Readiness,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SummaryRow::Role => "View",
            SummaryRow::Sources => "Sources",
            SummaryRow::Time => "Time",
            SummaryRow::Enrichment => "Enrichment",
            SummaryRow::Search => "Search",
            SummaryRow::Filter => "Advanced",
            SummaryRow::Grouping => "Grouping",
            SummaryRow::Fold => "Fold",
            SummaryRow::Columns => "Columns",
            SummaryRow::Colour => "Colour",
            SummaryRow::Readiness => "Readiness",
        }
    }

    /// Whether the row is an operation the user applies. The role, sources
    /// and readiness rows describe the view — every view reads from at least
    /// its own source — so they are never "not applied".
    pub fn is_operation(self) -> bool {
        !matches!(
            self,
            SummaryRow::Role | SummaryRow::Sources | SummaryRow::Readiness
        )
    }
}

/// The width of the label column: the longest label, so values align (§4.2).
const LABEL_WIDTH: usize = 10;

/// What a row reads as: the value its owning dialog would show, or `—`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SummaryEntry {
    pub row: SummaryRow,
    pub value: String,
    /// False when the operation is not applied and the value is the dash.
    pub applied: bool,
}

/// The rows for the active view, read fresh from `Views`. Pure: the same
/// function feeds `render`, the Enter handler and the tests.
pub fn summary_rows(
    views: &Views,
    sources: &[SourceItem],
    order: Option<ViewOrder>,
    ascii: bool,
) -> Vec<SummaryEntry> {
    let dash = if ascii { "-" } else { "—" };
    let Some(view) = views.active_item() else {
        return Vec::new();
    };
    let Some(state) = views.state(&view.id) else {
        return Vec::new();
    };
    let source_name = |id: &str| {
        sources
            .iter()
            .find(|source| source.id == id)
            .map_or_else(|| id.to_owned(), |source| source.name.clone())
    };
    SummaryRow::ALL
        .iter()
        .map(|row| {
            let value = match row {
                SummaryRow::Role => Some(match views.role(&view.id) {
                    ViewRole::Canonical => {
                        format!("All events of {} · fixed", source_name(&view.source_id))
                    }
                    ViewRole::Derived => format!("derived from {}", source_name(&view.source_id)),
                }),
                SummaryRow::Sources => {
                    let ids = views.source_ids(&view.id);
                    let names = ids.iter().map(|id| source_name(id)).collect::<Vec<_>>();
                    Some(if names.len() > 1 {
                        format!("{} (merged, {})", names.join(", "), order_value(order))
                    } else {
                        names.join(", ")
                    })
                }
                SummaryRow::Time => time_value(state),
                SummaryRow::Enrichment => enrichment_value(state, ascii),
                SummaryRow::Search => applied_text(&state.search.applied),
                SummaryRow::Filter => applied_text(&state.advanced.applied),
                SummaryRow::Grouping => applied_text(&state.grouping.applied).map(|value| {
                    match crate::grouping::parse_grouping(&value) {
                        Ok(crate::grouping::GroupingSpec::Auto) => "Auto".to_owned(),
                        Ok(crate::grouping::GroupingSpec::Custom(_)) => value,
                        Ok(crate::grouping::GroupingSpec::Run { column }) => {
                            format!("Run on {column}")
                        }
                        Ok(crate::grouping::GroupingSpec::Filter { column }) => {
                            format!("Starts on {column} non-null")
                        }
                        Err(_) => "Unsupported grouping version".to_owned(),
                    }
                }),
                SummaryRow::Fold => fold_value(state),
                SummaryRow::Columns => (!state.pinned_columns.is_empty())
                    .then(|| format!("pinned: {}", state.pinned_columns.join(", "))),
                SummaryRow::Colour => colour_value(state, ascii),
                SummaryRow::Readiness => Some(readiness_value(state)),
            };
            SummaryEntry {
                row: *row,
                applied: value.is_some(),
                value: value.unwrap_or_else(|| dash.to_owned()),
            }
        })
        .collect()
}

/// The same presentation-only ordering description the Time dialog uses.
/// Ordering is query-engine state; the summary only names what the provider
/// reports and never attempts to derive or evaluate it itself.
fn order_value(order: Option<ViewOrder>) -> String {
    match order {
        None => "capture (arrival)".to_owned(),
        Some(order) if !order.interleaved => format!(
            "{} · source order",
            time_basis_label(order.basis).to_lowercase()
        ),
        Some(order) if order.sources <= 1 => {
            format!("{} (arrival)", time_basis_label(order.basis).to_lowercase())
        }
        Some(order) if order.fully_ordered() => {
            format!("{} · merged", time_basis_label(order.basis).to_lowercase())
        }
        Some(order) => format!(
            "{} · merged · {} of {} sources arrive out of order",
            time_basis_label(order.basis).to_lowercase(),
            order.out_of_order,
            order.sources
        ),
    }
}

fn applied_text(applied: &str) -> Option<String> {
    let trimmed = applied.trim();
    (!trimmed.is_empty()).then(|| trimmed.replace('\n', " ⏎ "))
}

/// Applied row colouring in precedence order. Field colouring and predicate
/// rules are independent inputs, so neither hides the other. Rule position is
/// the editor's identity and the engine's precedence; the number makes that
/// order visible without inventing another identifier in this read-only view.
fn colour_value(state: &ViewState, ascii: bool) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(field) = &state.color_field {
        parts.push(format!("by {field}"));
    }
    if !state.color_rules.is_empty() {
        let arrow = if ascii { "->" } else { "→" };
        let rules = state
            .color_rules
            .iter()
            .enumerate()
            .map(|(index, rule)| {
                format!(
                    "{} {} {arrow} {}",
                    index + 1,
                    rule.predicate.replace('\n', " ⏎ "),
                    rule.color.label()
                )
            })
            .collect::<Vec<_>>();
        parts.push(format!(
            "{} rule{}: {}",
            rules.len(),
            if rules.len() == 1 { "" } else { "s" },
            rules.join(", ")
        ));
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// The Time dialog's Applied sentence, plus the basis it is measured in. A
/// window that is not applied under the default basis is not an operation.
fn time_value(state: &ViewState) -> Option<String> {
    let window = match state.applied_capture_time_policy {
        Some(CaptureTimePolicy::Recent { seconds }) => {
            Some(format!("rolling last {}", format_capture_duration(seconds)))
        }
        Some(CaptureTimePolicy::Absolute(_)) => Some(state.applied_capture_time.map_or_else(
            || "absolute pending".to_owned(),
            |window| {
                format!(
                    "absolute {} .. {}",
                    format_utc_nanos(window.start_unix_nanos),
                    format_utc_nanos(window.end_unix_nanos)
                )
            },
        )),
        None => None,
    };
    if window.is_none() && state.applied_time_basis == TimeBasis::Capture {
        return None;
    }
    let basis = match state.applied_time_basis {
        TimeBasis::Selected => state.applied_time_field.as_deref().map_or_else(
            || time_basis_label(TimeBasis::Selected).to_owned(),
            |field| format!("field {field}"),
        ),
        basis => time_basis_label(basis).to_owned(),
    };
    Some(format!(
        "{} · basis: {basis}",
        window.unwrap_or_else(|| "all times".to_owned())
    ))
}

/// The chain as the Enrichment list shows it: expression steps by their
/// source, command steps by glyph, name and run state (§12.5).
fn enrichment_value(state: &ViewState, ascii: bool) -> Option<String> {
    if state.enrichments.is_empty() {
        return None;
    }
    let glyph = if ascii { "$" } else { "⚙" };
    let steps = state
        .enrichments
        .iter()
        .map(|stage| {
            if stage.is_command() {
                let run = if state
                    .command_steps
                    .get(&stage.id.0)
                    .is_some_and(|run| run.publication.is_some())
                {
                    "results published"
                } else {
                    "unrun"
                };
                format!("{glyph} {} · {run}", stage.source)
            } else {
                stage.source.replace('\n', " ⏎ ")
            }
        })
        .collect::<Vec<_>>();
    let count = steps.len();
    Some(format!(
        "{count} step{}: {}",
        if count == 1 { "" } else { "s" },
        steps.join(", ")
    ))
}

/// The Folding dialog's settings on one line (§12.19). Normalisation is named
/// only for the derived pattern key, exactly as the dialog draws it.
fn fold_value(state: &ViewState) -> Option<String> {
    if !state.fold_enabled {
        return None;
    }
    let scope = match state.fold_lookback {
        0 => "adjacent".to_owned(),
        1 => "lookback 1 row".to_owned(),
        rows => format!("lookback {rows} rows"),
    };
    let minimum_run = if state.fold_minimum_run == 0 {
        DEFAULT_FOLD_MINIMUM_RUN
    } else {
        state.fold_minimum_run
    };
    let mut value = format!(
        "by {} · {scope} · min {minimum_run}",
        fold_key_label(state.fold_key_column.as_deref()),
    );
    if state.fold_key_column.is_none() {
        value.push_str(" · ");
        value.push_str(&state.fold_normalisation.label().to_ascii_lowercase());
    }
    Some(value)
}

/// Whether the rows on screen are the applied definition. Worded like the
/// status line and the message rows of the dialogs that own each state.
fn readiness_value(state: &ViewState) -> String {
    let query_pending = state.search.pending_generation.is_some()
        || state.advanced.pending_generation.is_some()
        || state.enrichment.pending_generation.is_some()
        || state.grouping.pending_generation.is_some()
        || state.applied_query_revision != state.desired_query_revision;
    if query_pending {
        return "query pending · the last applied definition stays active".to_owned();
    }
    if state.time_update_pending() {
        return "time window updating · the last applied window stays active".to_owned();
    }
    let stale = stale_command_steps(state);
    if !stale.is_empty() {
        return format!(
            "{} command step{} unrun: {}",
            stale.len(),
            if stale.len() == 1 { "" } else { "s" },
            stale.join(", ")
        );
    }
    "ready".to_owned()
}

/// Which control has focus: the list, or the one button.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SummaryControl {
    List,
    Open,
}

/// Everything the layer draws that can be clicked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SummaryHit {
    Row(usize),
    Control(SummaryControl),
    Body,
}

#[derive(Clone, Debug, Default)]
struct SummaryGeometry {
    body: Rect,
    rows: Vec<(Rect, usize)>,
    controls: Vec<(Rect, SummaryControl)>,
}

#[derive(Debug)]
pub struct ViewSummaryDialog {
    open: bool,
    selected: usize,
    control: SummaryControl,
    geometry: SummaryGeometry,
    surface: Surface,
}

impl Default for ViewSummaryDialog {
    fn default() -> Self {
        Self {
            open: false,
            selected: 0,
            control: SummaryControl::List,
            geometry: SummaryGeometry::default(),
            surface: Surface::default(),
        }
    }
}

const CONTROLS: [SummaryControl; 2] = [SummaryControl::List, SummaryControl::Open];

/// The one button's label; `&` marks the letter that presses it (§8.10).
const OPEN_LABEL: &str = "&Open";

impl ViewSummaryDialog {
    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn selected_row(&self) -> SummaryRow {
        SummaryRow::ALL[self.selected.min(SummaryRow::ALL.len() - 1)]
    }

    pub fn control(&self) -> SummaryControl {
        self.control
    }

    /// §8.9: the one default, named here for both the fill and the Enter arm.
    pub fn default_control() -> SummaryControl {
        SummaryControl::Open
    }

    fn move_selection(&mut self, delta: i32) {
        let last = SummaryRow::ALL.len() as i32 - 1;
        self.selected = (self.selected as i32 + delta).clamp(0, last) as usize;
    }

    /// The layer the selected row hands over to, with its item selected. This
    /// is the whole reason the layer exists, so it is one function the Enter
    /// arm, the button and the mouse all call.
    fn open_selected(&mut self, ctx: &mut Ctx<'_>) -> Outcome {
        let Some(state) = ctx.views.active() else {
            return Outcome::Consumed;
        };
        let open = match self.selected_row() {
            SummaryRow::Role => Open::View,
            SummaryRow::Sources => Open::ViewMembership,
            SummaryRow::Time => Open::Time,
            SummaryRow::Enrichment => Open::Enrichment,
            SummaryRow::Search => Open::Search,
            SummaryRow::Filter => Open::Advanced,
            SummaryRow::Grouping => Open::Grouping,
            SummaryRow::Fold => Open::Grouping,
            SummaryRow::Columns => match state.pinned_columns.first() {
                Some(column) => Open::FieldColumn {
                    column: column.clone(),
                },
                None => Open::Fields,
            },
            SummaryRow::Colour if !state.color_rules.is_empty() => Open::ColorRules,
            SummaryRow::Colour => match &state.color_field {
                Some(column) => Open::FieldColumn {
                    column: column.clone(),
                },
                None => Open::ColorRules,
            },
            // Readiness is owned by whatever is not current: an unrun command
            // step is the Enrichment list's to run, and everything else
            // resolves on its own, so the view itself is the fallback.
            SummaryRow::Readiness => {
                let stale = stale_command_steps(state);
                if stale.is_empty() {
                    Open::View
                } else {
                    Open::Enrichment
                }
            }
        };
        // Enrichment opens on the step the chain is about: the first stale
        // command step when readiness sent us there, otherwise the selection
        // the list already holds (clamped by the list on open).
        if open == Open::Enrichment
            && self.selected_row() == SummaryRow::Readiness
            && let Some(state) = ctx.views.active_mut()
        {
            let stale = stale_command_steps(state);
            if let Some(index) = state
                .enrichments
                .iter()
                .position(|stage| stale.contains(&stage.source))
            {
                state.enrichment_selected = index;
            }
        }
        self.open = false;
        Outcome::Replace(open)
    }

    fn key(&mut self, key: KeyEvent, ctx: &mut Ctx<'_>) -> Outcome {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return Outcome::Ignored;
        }
        match key.code {
            KeyCode::Up => {
                self.move_selection(-1);
                Outcome::Consumed
            }
            KeyCode::Down => {
                self.move_selection(1);
                Outcome::Consumed
            }
            KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.control = move_control(self.control, &CONTROLS, -1);
                Outcome::Consumed
            }
            KeyCode::BackTab => {
                self.control = move_control(self.control, &CONTROLS, -1);
                Outcome::Consumed
            }
            KeyCode::Tab => {
                self.control = move_control(self.control, &CONTROLS, 1);
                Outcome::Consumed
            }
            // §8.9: the list and the button both hand Enter to the default.
            // The button's letter (`o`, bare or with Alt) is a §8.10 mnemonic
            // the shell resolves through `action_labels` before the key
            // arrives here; nothing in this layer types, so it is always live.
            KeyCode::Enter => self.open_selected(ctx),
            _ => Outcome::Ignored,
        }
    }

    fn mouse(
        &mut self,
        kind: MouseEventKind,
        hit: Option<SummaryHit>,
        ctx: &mut Ctx<'_>,
    ) -> Outcome {
        match kind {
            MouseEventKind::Down(MouseButton::Left) => match hit {
                Some(SummaryHit::Row(index)) => {
                    self.selected = index.min(SummaryRow::ALL.len() - 1);
                    self.control = SummaryControl::List;
                    Outcome::Consumed
                }
                Some(SummaryHit::Control(SummaryControl::Open)) => self.open_selected(ctx),
                Some(SummaryHit::Control(SummaryControl::List)) => {
                    self.control = SummaryControl::List;
                    Outcome::Consumed
                }
                _ => Outcome::Consumed,
            },
            MouseEventKind::ScrollUp => {
                self.move_selection(-1);
                Outcome::Consumed
            }
            MouseEventKind::ScrollDown => {
                self.move_selection(1);
                Outcome::Consumed
            }
            _ => Outcome::Ignored,
        }
    }
}

fn contains(area: Rect, point: (u16, u16)) -> bool {
    point.0 >= area.x && point.0 < area.right() && point.1 >= area.y && point.1 < area.bottom()
}

impl Component for ViewSummaryDialog {
    type Hit = SummaryHit;
    type Open = ();

    fn open(&mut self, _params: (), _ctx: &mut Ctx<'_>) {
        self.open = true;
        // §8.5: a list opens on a real row, and this one always has rows. The
        // first row is the view itself, which is what the title names.
        self.selected = 0;
        self.control = SummaryControl::List;
        self.geometry = SummaryGeometry::default();
        self.surface = Surface::default();
    }

    fn handle(&mut self, event: Event<SummaryHit>, ctx: &mut Ctx<'_>) -> Outcome {
        match event {
            Event::Key(key) => self.key(key, ctx),
            Event::Mouse { kind, hit, .. } => self.mouse(kind, hit, ctx),
            Event::Dismiss => {
                self.open = false;
                Outcome::Close
            }
            // Every value is re-read from `Views` on the next frame, so a view
            // event needs no reaction beyond the redraw the shell already does.
            Event::View(_) => Outcome::Consumed,
            Event::Command(_) | Event::Paste(_) | Event::Resize => Outcome::Ignored,
        }
    }

    /// §8.10: the one button, as `render` draws it, so the shell can press it
    /// from its underlined letter.
    fn action_labels(&self, _ctx: &Ctx<'_>) -> Vec<&'static str> {
        vec![OPEN_LABEL]
    }

    fn press_action(&mut self, index: usize, ctx: &mut Ctx<'_>) -> Outcome {
        if index == 0 {
            self.open_selected(ctx)
        } else {
            Outcome::Ignored
        }
    }

    fn surface(&self) -> Surface {
        self.surface
    }

    fn hit(&self, point: (u16, u16)) -> Option<SummaryHit> {
        let g = &self.geometry;
        g.controls
            .iter()
            .find_map(|(rect, control)| {
                contains(*rect, point).then_some(SummaryHit::Control(*control))
            })
            .or_else(|| {
                g.rows.iter().find_map(|(rect, index)| {
                    contains(*rect, point).then_some(SummaryHit::Row(*index))
                })
            })
            .or_else(|| contains(g.body, point).then_some(SummaryHit::Body))
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx<'_>) -> Surface {
        use crate::dialog_layout::{DialogClass, DialogContent, content_width, pane};

        let theme = ctx.theme;
        let ascii = ctx.ascii;
        let styles = DialogStyles::new(theme);
        let order = ctx
            .views
            .active_id()
            .and_then(|view_id| ctx.provider.view_order(view_id));
        let entries = summary_rows(ctx.views, ctx.sources, order, ascii);
        let total = entries.len();
        let selected = self.selected.min(total.saturating_sub(1));
        let labels = [OPEN_LABEL];
        let width = content_width(area, DialogClass::M);

        // §7.4: one message row. The state is what the readiness row says;
        // the sentence counts what is applied.
        let operations = entries.iter().filter(|entry| entry.row.is_operation());
        let applied = operations.clone().filter(|entry| entry.applied).count();
        let available = operations.count();
        let pending = entries
            .iter()
            .find(|entry| entry.row == SummaryRow::Readiness)
            .is_some_and(|entry| entry.value != "ready");
        let (state, sentence) = if pending {
            (
                MessageState::Updating,
                format!(
                    "{applied} of {available} operations applied · not all of them are current"
                ),
            )
        } else if applied == 0 {
            (
                MessageState::Ready,
                "no operation applied · every record of the sources is shown".to_owned(),
            )
        } else {
            (
                MessageState::Applied,
                format!("{applied} of {available} operations applied"),
            )
        };
        let help = "Rows follow the order the view evaluates them.";

        // §5.2: the body is the list pane — heading plus one row per entry,
        // which is a fixed count — so height follows stable content.
        let body_rows = 1 + u16::try_from(total.max(1)).unwrap_or(1);
        let content = DialogContent {
            header: 0,
            body: body_rows,
            message: message_rows(&sentence, width),
            help: help_rows(help, width),
            actions: packed_button_rows(width, &labels),
        };
        let title = match ctx.views.active_item() {
            Some(view) => format!("View summary · {}", view.name),
            None => "View summary".to_owned(),
        };
        let regions = dialog_frame_regions(frame, area, DialogClass::M, &title, &content, theme);
        let surface = Surface {
            popup: regions.popup,
            interior: regions.interior,
            caret: None,
            scrollable: true,
            text_focus: false,
        };
        self.geometry = SummaryGeometry {
            body: regions.body,
            ..SummaryGeometry::default()
        };
        self.surface = surface;
        if regions.content.width == 0 || !ctx.active {
            return surface;
        }

        let mut rows: Vec<(Rect, usize)> = Vec::new();
        let mut controls: Vec<(Rect, SummaryControl)> = Vec::new();

        // §8.5/§8.7: a list is a pane with a heading, a count and a scrollbar
        // only when the rows do not fit.
        let rects = pane(regions.body, 12, total);
        frame.render_widget(
            Paragraph::new("Operations").style(styles.label.add_modifier(Modifier::BOLD)),
            rects.heading,
        );
        if rects.count.width > 0 && total > 0 {
            frame.render_widget(
                Paragraph::new(Line::from(format!("{} of {total}", selected + 1)))
                    .style(styles.description)
                    .right_aligned(),
                rects.count,
            );
        }
        let visible = usize::from(rects.viewport.height);
        // §9: the viewport windows on the selection.
        let first = selected
            .saturating_sub(visible.saturating_sub(1))
            .min(total.saturating_sub(visible.min(total)));
        for (offset, (index, entry)) in entries
            .iter()
            .enumerate()
            .skip(first)
            .take(visible)
            .enumerate()
        {
            let is_selected = index == selected;
            let marker = if is_selected {
                if ascii { "> " } else { "› " }
            } else {
                "  "
            };
            let label = entry.row.label();
            let padding = LABEL_WIDTH.saturating_sub(UnicodeWidthStr::width(label));
            let separator = if ascii { " . " } else { " · " };
            let text = format!(
                "{marker}{label}{}{separator}{}",
                " ".repeat(padding),
                entry.value
            );
            let row = Rect::new(
                rects.viewport.x,
                rects.viewport.y.saturating_add(offset as u16),
                rects.viewport.width,
                1,
            );
            let style = if is_selected && self.control == SummaryControl::List {
                styles.selection
            } else if is_selected {
                styles.label.add_modifier(Modifier::BOLD)
            } else if entry.applied {
                styles.label
            } else {
                styles.description
            };
            // §9: a long value truncates with a trailing `…`, never mid-glyph.
            frame.render_widget(
                Paragraph::new(truncated(&text, usize::from(row.width))).style(style),
                row,
            );
            rows.push((row, index));
        }
        if let Some(bar) = rects.scrollbar {
            render_scrollbar(
                frame,
                bar,
                first,
                total.saturating_sub(visible),
                theme,
                ascii,
            );
        }

        render_message(frame, regions.message, state, &sentence, theme, ascii);
        render_help_text(frame, regions.help, help, theme);

        // §8.9: one button, and it is the default.
        let focused = (self.control == SummaryControl::Open).then_some(0);
        for (index, rect) in render_actions(
            frame,
            regions.actions,
            ActionRow {
                labels: &labels,
                default: Some(0),
                destructive: &[],
                focused,
            },
            theme,
        ) {
            debug_assert_eq!(index, 0);
            controls.push((rect, Self::default_control()));
        }

        self.geometry = SummaryGeometry {
            body: regions.body,
            rows,
            controls,
        };
        self.surface = surface;
        surface
    }
}
