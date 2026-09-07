//! The Folding layer: which column a view's runs fold on, and the policy
//! around it (`docs/dialog-system.md` §3–§8, `docs/component-model.md` §1).
//!
//! The model this dialog exposes is deliberately small: **a run is a group of
//! consecutive rows sharing one key, and the key is the value of exactly one
//! column.** The default column is a derived one, `Message pattern`, which is
//! the row text with volatile substrings replaced and the level prefixed —
//! precisely what folding keyed on before a column could be chosen, so a view
//! that never opens this dialog folds exactly as it always did. Any other
//! column, including an enrichment column, supplies its value as-is: nothing is
//! normalised, and a field the user did not ask about is never replaced. That
//! is why `Normalisation` is only shown while the key is the derived column —
//! it is the only thing it can affect.
//!
//! Folding on *several* fields is therefore not a second mechanism here. The
//! key column picker offers `[ New column… ]`, which asks which fields to
//! combine and opens the enrichment step editor on a concatenation of them
//! (`Open::EnrichmentStep { prefill }`). The user reviews and saves an ordinary
//! enrichment step; when the view accepts it, this layer selects the column it
//! created. There is no second field-combination mechanism to keep in sync.
//!
//! Every control writes the view immediately rather than through an `Apply`
//! button. Folding is reversible presentation over rows that are never touched,
//! `fold_request` is recomputed from `ViewState` every frame, and Escape is not
//! an undo (`docs/component-model.md` §6.5).

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    text::Line,
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Widget},
};
use unicode_width::UnicodeWidthStr;

use crate::app::{
    FOLD_LOOKBACK_CHOICES, FOLD_MINIMUM_RUN_CHOICES, QueryPurpose, Views, move_control,
};
use crate::component::{Component, Ctx, Event, Open, Outcome, RenderCtx, Surface, ViewEvent};
use crate::dialog_controls::{ActionRow, DialogStyles};
use crate::dialog_layout::MIN_LIVE_ROWS;
use crate::provider::{FoldNormalisation, RowProvider, ViewportRequest};
use crate::ui::{
    FIELD_GUTTER, InputSurface, MessageState, help_rows, message_rows, packed_button_rows,
    render_actions, render_help_text, render_message, truncated,
};

/// Rows sampled to discover which columns a view actually carries. The same
/// bound the editor completion uses, and read through `unfolded_page`, because
/// folding is presentation and must never change what is sampled.
const FOLD_COLUMN_SAMPLE_ROWS: usize = 128;
/// Columns offered in the picker. More than this is a schema to browse, not a
/// choice to make in one dropdown.
const FOLD_MAX_COLUMNS: usize = 64;
/// Named values read from one sampled row. Mirrors the editor completion's
/// per-row bound.
const FOLD_MAX_ROW_FIELDS: usize = 128;
/// Fields one generated column may concatenate.
const FOLD_MAX_COMPOSED_FIELDS: usize = 8;
/// The name a generated fold column takes, before disambiguation.
const FOLD_COLUMN_NAME: &str = "fold_key";
/// §5.1 caps a class A popup at eight rows; that is what the picker asks for.
const FOLD_PICKER_ROWS: u16 = 8;

/// §5.2.1: how many rows an open picker reserves, from the frame alone.
///
/// The key-column list is a live region. Its content comes from a bounded
/// sample of the view's rows, so a source that is still arriving can add a
/// column while the list is open, and the compose list gains and loses nothing
/// but its checkmarks. `anchored_rect` sizes a class A popup from its item
/// count, which would move the list's rows under the cursor when that happens;
/// reserving the rows instead makes the popup rect identical from one frame to
/// the next. An overlong list scrolls behind a trailing `+N more`, which is the
/// §5.2.1 affordance for a region with no pane heading; a short one leaves the
/// remaining rows blank.
///
/// This is not `dialog_layout::live_rows`, whose spare-row arithmetic is for a
/// region *inside* a body the dialog has to fit. An anchored popup is bounded
/// by the frame and may extend past the dialog it belongs to (§10), so the
/// frame is what the reservation comes from — with the same `MIN_LIVE_ROWS`
/// floor, because a one-row list is not a list.
fn picker_rows(area: Rect) -> usize {
    // Border rows, plus one row of anchor above and below.
    let available = area.height.saturating_sub(4);
    usize::from(
        available
            .min(FOLD_PICKER_ROWS)
            .max(MIN_LIVE_ROWS.min(available)),
    )
}

/// How the derived column is named wherever the user meets it.
pub const PATTERN_COLUMN_LABEL: &str = "Message pattern";

/// Which control has focus. UI-only state, so it lives here (§7.3).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FoldingControl {
    Enabled,
    KeyColumn,
    MinimumRun,
    Scope,
    Normalisation,
    Collapse,
}

/// Which anchored list is open, if any.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FoldingDropdown {
    KeyColumn,
    MinimumRun,
    Scope,
    Normalisation,
}

/// Everything the layer draws that can be clicked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FoldingHit {
    Control(FoldingControl),
    Choice(usize),
    Body,
}

/// Recorded by `render`, consumed by `hit()` (§5.1).
#[derive(Clone, Debug, Default)]
struct FoldingGeometry {
    body: Rect,
    controls: Vec<(Rect, FoldingControl)>,
    choices: Vec<(Rect, usize)>,
}

#[derive(Debug, Default)]
pub struct FoldingDialog {
    open: bool,
    control: Option<FoldingControl>,
    dropdown: Option<FoldingDropdown>,
    highlighted: usize,
    /// While `Some`, the key-column list is a checkbox list of the fields a new
    /// enrichment column would concatenate, in the order they were picked.
    composing: Option<Vec<String>>,
    /// The column name handed to the step editor, until the view accepts a
    /// stage that produces it. Fenced on the view, and on the accepted chain
    /// actually containing the stage, so a cancelled editor or an unrelated
    /// enrichment never silently changes the fold key.
    pending_column: Option<(String, String)>,
    message: Option<(MessageState, String)>,
    geometry: FoldingGeometry,
    surface: Surface,
}

/// The columns this view's rows carry, discovered from a bounded sample.
///
/// Read through [`RowProvider::unfolded_page`]: folding is presentation, and
/// anything that samples rows must see the view as if it were off, so that
/// turning folding on cannot change which columns are offered to fold on.
pub fn fold_columns(views: &Views, provider: &dyn RowProvider) -> Vec<String> {
    let Some(view_id) = views.active_id() else {
        return Vec::new();
    };
    let anchor = views
        .active()
        .and_then(|state| state.selected.as_ref())
        .and_then(|id| provider.index_of_id(view_id, id))
        .unwrap_or(0);
    let page = provider.unfolded_page(
        view_id,
        ViewportRequest {
            start: anchor,
            len: FOLD_COLUMN_SAMPLE_ROWS,
        },
    );
    let mut names: Vec<String> = Vec::new();
    for row in page.rows {
        for (field, _) in row.fields.into_iter().take(FOLD_MAX_ROW_FIELDS) {
            if names.len() >= FOLD_MAX_COLUMNS {
                break;
            }
            if !names.iter().any(|known| known == &field) {
                names.push(field);
            }
        }
    }
    names.sort();
    // A key the view already folds on stays offered even when the sample does
    // not happen to contain it; otherwise the picker would silently disagree
    // with the row the dialog is showing.
    if let Some(current) = views
        .active()
        .and_then(|state| state.fold_key_column.clone())
        && !names.iter().any(|known| known == &current)
    {
        names.insert(0, current);
    }
    names
}

/// The label the picker shows for a key, and the message row quotes.
pub fn fold_key_label(column: Option<&str>) -> String {
    match column {
        None => PATTERN_COLUMN_LABEL.to_owned(),
        Some(name) => name.to_owned(),
    }
}

fn scope_label(lookback: usize) -> String {
    match lookback {
        0 => "Adjacent runs only".to_owned(),
        1 => "Lookback 1 row".to_owned(),
        window => format!("Lookback {window} rows"),
    }
}

/// The enrichment step a `[ New column… ]` choice opens the editor on.
///
/// One `pl.concat_str` over the chosen fields. It is a draft: the user reads it
/// in the step editor, edits it if the separator or the null handling is wrong
/// for their data, and saves it as an ordinary step.
pub fn composed_column_source(name: &str, fields: &[String]) -> String {
    let parts = fields
        .iter()
        .map(|field| format!("pl.col({}).cast(pl.String)", python_literal(field)))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{name} = pl.concat_str([{parts}], separator=\"|\", ignore_nulls=True)")
}

/// A single-quoted Python string literal for a column name.
fn python_literal(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for character in value.chars() {
        match character {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            other => out.push(other),
        }
    }
    out.push('\'');
    out
}

/// A generated column name that does not collide with a column the view already
/// carries or a step it already applies.
fn unused_column_name(taken: &[String]) -> String {
    if !taken.iter().any(|name| name == FOLD_COLUMN_NAME) {
        return FOLD_COLUMN_NAME.to_owned();
    }
    for suffix in 2..=99u32 {
        let candidate = format!("{FOLD_COLUMN_NAME}_{suffix}");
        if !taken.iter().any(|name| name == &candidate) {
            return candidate;
        }
    }
    format!("{FOLD_COLUMN_NAME}_x")
}

fn contains(area: Rect, point: (u16, u16)) -> bool {
    point.0 >= area.x && point.0 < area.right() && point.1 >= area.y && point.1 < area.bottom()
}

impl FoldingDialog {
    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn control(&self) -> Option<FoldingControl> {
        self.control
    }

    /// Geometry recorded by the last `render`; `hit()` is how input reaches it.
    pub fn control_rects(&self) -> &[(Rect, FoldingControl)] {
        &self.geometry.controls
    }

    pub fn choice_rects(&self) -> &[(Rect, usize)] {
        &self.geometry.choices
    }

    /// The row the open list has under the cursor.
    pub fn highlighted(&self) -> usize {
        self.highlighted
    }

    /// Whether the picker is asking which fields a new column combines.
    pub fn composing(&self) -> Option<&[String]> {
        self.composing.as_deref()
    }

    /// Tab order: the fields top to bottom, then the action. Normalisation is
    /// absent while the key is a real column, because it cannot affect one.
    fn controls(&self, pattern_key: bool) -> Vec<FoldingControl> {
        let mut controls = vec![
            FoldingControl::Enabled,
            FoldingControl::KeyColumn,
            FoldingControl::MinimumRun,
            FoldingControl::Scope,
        ];
        if pattern_key {
            controls.push(FoldingControl::Normalisation);
        }
        controls.push(FoldingControl::Collapse);
        controls
    }

    fn pattern_key(&self, ctx: &Ctx<'_>) -> bool {
        ctx.views
            .active()
            .is_none_or(|state| state.fold_key_column.is_none())
    }

    fn focused(&self, ctx: &Ctx<'_>) -> FoldingControl {
        let controls = self.controls(self.pattern_key(ctx));
        match self.control {
            Some(control) if controls.contains(&control) => control,
            _ => controls[0],
        }
    }

    /// The rows the open dropdown lists, and the row that is currently the
    /// value. Rendering, keyboard selection and hit-testing share it.
    fn choices(
        &self,
        dropdown: FoldingDropdown,
        views: &Views,
        provider: &dyn RowProvider,
    ) -> (Vec<String>, usize) {
        let state = views.active();
        match dropdown {
            FoldingDropdown::KeyColumn => {
                let columns = fold_columns(views, provider);
                if let Some(picked) = &self.composing {
                    let rows = columns
                        .iter()
                        .map(|column| {
                            let order = picked.iter().position(|name| name == column);
                            let mark = if order.is_some() { "[x]" } else { "[ ]" };
                            match order {
                                Some(index) => format!("{mark} {:>2}  {column}", index + 1),
                                None => format!("{mark}      {column}"),
                            }
                        })
                        .collect::<Vec<_>>();
                    return (rows, 0);
                }
                let current = state.and_then(|state| state.fold_key_column.clone());
                let selected = match &current {
                    None => 0,
                    Some(name) => columns
                        .iter()
                        .position(|column| column == name)
                        .map_or(0, |index| index + 1),
                };
                let mut rows = vec![format!("{PATTERN_COLUMN_LABEL}   (default)")];
                rows.extend(columns);
                rows.push("[ New column… ]".to_owned());
                (rows, selected)
            }
            FoldingDropdown::MinimumRun => {
                let current = state.map_or(0, |state| state.fold_minimum_run);
                (
                    FOLD_MINIMUM_RUN_CHOICES
                        .iter()
                        .map(|run| format!("{run} or more"))
                        .collect(),
                    FOLD_MINIMUM_RUN_CHOICES
                        .iter()
                        .position(|run| *run == current)
                        .unwrap_or(1),
                )
            }
            FoldingDropdown::Scope => {
                let current = state.map_or(0, |state| state.fold_lookback);
                (
                    FOLD_LOOKBACK_CHOICES
                        .iter()
                        .copied()
                        .map(scope_label)
                        .collect(),
                    FOLD_LOOKBACK_CHOICES
                        .iter()
                        .position(|window| *window == current)
                        .unwrap_or(0),
                )
            }
            FoldingDropdown::Normalisation => {
                let current =
                    state.map_or_else(FoldNormalisation::default, |state| state.fold_normalisation);
                (
                    FoldNormalisation::ALL
                        .iter()
                        .map(|mode| mode.label().to_owned())
                        .collect(),
                    FoldNormalisation::ALL
                        .iter()
                        .position(|mode| *mode == current)
                        .unwrap_or(1),
                )
            }
        }
    }

    /// Every change lands on the view straight away: folding is reversible
    /// presentation, so there is nothing to stage and no `Apply` to press.
    fn edit(&mut self, ctx: &mut Ctx<'_>, apply: impl FnOnce(&mut crate::app::ViewState)) {
        let Some(view_id) = ctx.views.active_id().map(str::to_owned) else {
            return;
        };
        if let Some(state) = ctx.views.state_mut(&view_id) {
            apply(state);
        }
        ctx.views.touch(&view_id);
    }

    fn toggle_enabled(&mut self, ctx: &mut Ctx<'_>) {
        self.edit(ctx, |state| {
            state.fold_enabled = !state.fold_enabled;
            if state.fold_minimum_run == 0 {
                state.fold_minimum_run = crate::app::DEFAULT_FOLD_MINIMUM_RUN;
            }
            if !state.fold_enabled {
                // Expansion names runs of the policy that produced them; a
                // policy the user switched off has none.
                state.fold_expanded.clear();
            }
        });
        self.message = None;
    }

    /// Collapse every expanded run again. The only action in the row, because
    /// every other control takes effect where it stands.
    fn collapse_all(&mut self, ctx: &mut Ctx<'_>) {
        let enabled = ctx.views.active().is_some_and(|state| state.fold_enabled);
        if !enabled {
            self.message = Some((
                MessageState::Disabled,
                "folding is off for this view; nothing is collapsed".to_owned(),
            ));
            return;
        }
        self.edit(ctx, |state| state.fold_expanded.clear());
        self.message = Some((
            MessageState::Applied,
            "every repeated run is collapsed again".to_owned(),
        ));
    }

    fn open_dropdown(&mut self, dropdown: FoldingDropdown, ctx: &Ctx<'_>) -> Outcome {
        self.dropdown = Some(dropdown);
        self.composing = None;
        self.highlighted = self.choices(dropdown, ctx.views, ctx.provider).1;
        Outcome::Consumed
    }

    /// §8.9: the one default action, named in the one place `render` (which
    /// button to fill) and the Enter arm (which verb to run) both read.
    ///
    /// Folding is a settings dialog — every field takes effect where it stands,
    /// so there is no `Apply` — and `Collapse expanded runs` is the only verb
    /// in the row. It is therefore the default, and it is filled. §8.9 reserves
    /// "no default" for a dialog with no action row at all (Help); a dialog
    /// that draws a button and refuses to call it the default would leave the
    /// row unmarked for no reason.
    ///
    /// What the declaration buys here is smaller than usual, and worth saying
    /// plainly: every control in this body is a checkbox or a closed dropdown,
    /// and §8.9's own table gives Enter to each of those, so nothing hands
    /// Enter on today. The default is still read from one function, so a
    /// non-consuming control added later cannot disagree with the fill.
    fn default_control() -> FoldingControl {
        FoldingControl::Collapse
    }

    /// Run the default action, from wherever Enter reached it.
    fn run_default(&mut self, ctx: &mut Ctx<'_>) -> Outcome {
        self.activate_control(Self::default_control(), ctx)
    }

    /// Enter on the focused control, per §8.9's exception table: a checkbox
    /// toggles, a closed dropdown opens, a button presses itself, and anything
    /// else hands Enter to the default.
    fn activate(&mut self, ctx: &mut Ctx<'_>) -> Outcome {
        self.activate_control(self.focused(ctx), ctx)
    }

    fn activate_control(&mut self, control: FoldingControl, ctx: &mut Ctx<'_>) -> Outcome {
        match control {
            FoldingControl::Enabled => {
                self.toggle_enabled(ctx);
                Outcome::Consumed
            }
            FoldingControl::KeyColumn => self.open_dropdown(FoldingDropdown::KeyColumn, ctx),
            FoldingControl::MinimumRun => self.open_dropdown(FoldingDropdown::MinimumRun, ctx),
            FoldingControl::Scope => self.open_dropdown(FoldingDropdown::Scope, ctx),
            FoldingControl::Normalisation => {
                self.open_dropdown(FoldingDropdown::Normalisation, ctx)
            }
            FoldingControl::Collapse => {
                self.collapse_all(ctx);
                Outcome::Consumed
            }
        }
    }

    /// Enter inside the open dropdown.
    fn choose(&mut self, ctx: &mut Ctx<'_>) -> Outcome {
        let Some(dropdown) = self.dropdown else {
            return Outcome::Ignored;
        };
        let index = self.highlighted;
        match dropdown {
            FoldingDropdown::KeyColumn if self.composing.is_some() => self.compose(ctx),
            FoldingDropdown::KeyColumn => {
                let columns = fold_columns(ctx.views, ctx.provider);
                if index == 0 {
                    self.edit(ctx, |state| state.fold_key_column = None);
                } else if let Some(column) = columns.get(index - 1).cloned() {
                    self.edit(ctx, move |state| state.fold_key_column = Some(column));
                } else if columns.is_empty() {
                    // Nothing to combine: say so rather than opening an empty
                    // checkbox list the user cannot act on.
                    self.message = Some((
                        MessageState::Disabled,
                        "this view carries no columns to combine yet".to_owned(),
                    ));
                    self.dropdown = None;
                    return Outcome::Consumed;
                } else {
                    // The trailing `[ New column… ]` row.
                    self.composing = Some(Vec::new());
                    self.highlighted = 0;
                    self.message = Some((
                        MessageState::Ready,
                        // §8.10: the message says what is happening; the keys that pick
                        // and build are the conventions Help states once.
                        "choosing the fields that make up the new column".to_owned(),
                    ));
                    return Outcome::Consumed;
                }
                // A changed key repartitions the stream, so the runs the user
                // expanded under the old key no longer name anything.
                self.edit(ctx, |state| state.fold_expanded.clear());
                self.dropdown = None;
                self.message = None;
                Outcome::Consumed
            }
            FoldingDropdown::MinimumRun => {
                if let Some(run) = FOLD_MINIMUM_RUN_CHOICES.get(index).copied() {
                    self.edit(ctx, move |state| state.fold_minimum_run = run);
                }
                self.dropdown = None;
                Outcome::Consumed
            }
            FoldingDropdown::Scope => {
                if let Some(window) = FOLD_LOOKBACK_CHOICES.get(index).copied() {
                    self.edit(ctx, move |state| state.fold_lookback = window);
                }
                self.dropdown = None;
                Outcome::Consumed
            }
            FoldingDropdown::Normalisation => {
                if let Some(mode) = FoldNormalisation::ALL.get(index).copied() {
                    self.edit(ctx, move |state| state.fold_normalisation = mode);
                }
                self.dropdown = None;
                Outcome::Consumed
            }
        }
    }

    /// Space inside the `[ New column… ]` checkbox list.
    fn toggle_composed(&mut self, ctx: &Ctx<'_>) -> Outcome {
        let columns = fold_columns(ctx.views, ctx.provider);
        let Some(column) = columns.get(self.highlighted).cloned() else {
            return Outcome::Consumed;
        };
        let Some(picked) = self.composing.as_mut() else {
            return Outcome::Ignored;
        };
        if let Some(index) = picked.iter().position(|name| name == &column) {
            picked.remove(index);
        } else if picked.len() < FOLD_MAX_COMPOSED_FIELDS {
            picked.push(column);
        } else {
            self.message = Some((
                MessageState::Error,
                format!("at most {FOLD_MAX_COMPOSED_FIELDS} fields in one fold column"),
            ));
        }
        Outcome::Consumed
    }

    /// Hand the chosen fields to the enrichment step editor as one expression.
    /// This layer stays on the stack under the child and adopts the column when
    /// the view accepts the step.
    fn compose(&mut self, ctx: &mut Ctx<'_>) -> Outcome {
        let picked = self.composing.clone().unwrap_or_default();
        if picked.is_empty() {
            self.message = Some((
                MessageState::Error,
                "pick at least one field to combine".to_owned(),
            ));
            return Outcome::Consumed;
        }
        let Some(view_id) = ctx.views.active_id().map(str::to_owned) else {
            return Outcome::Consumed;
        };
        let mut taken = fold_columns(ctx.views, ctx.provider);
        if let Some(state) = ctx.views.state(&view_id) {
            taken.extend(
                state
                    .enrichments
                    .iter()
                    .filter_map(|stage| stage.source.split_once('=').map(|(name, _)| name.trim()))
                    .map(str::to_owned),
            );
        }
        let name = unused_column_name(&taken);
        let source = composed_column_source(&name, &picked);
        self.composing = None;
        self.dropdown = None;
        self.message = Some((
            MessageState::Pending,
            format!("save the step to fold on {name}"),
        ));
        self.pending_column = Some((view_id, name));
        Outcome::OpenChild(Open::EnrichmentStep {
            editing: None,
            prefill: Some(source),
        })
    }

    /// The view accepted an enrichment. Adopt the column only when the accepted
    /// chain really produces the name this layer asked for: a cancelled editor,
    /// or someone else's step, must not change the fold key.
    fn adopt_pending(&mut self, view_id: &str, ctx: &mut Ctx<'_>) -> Outcome {
        let Some((pending_view, name)) = self.pending_column.clone() else {
            return Outcome::Ignored;
        };
        if pending_view != view_id {
            return Outcome::Ignored;
        }
        let produced = ctx.views.state(view_id).is_some_and(|state| {
            state.enrichments.iter().any(|stage| {
                stage
                    .source
                    .split_once('=')
                    .is_some_and(|(field, _)| field.trim() == name)
            })
        });
        if !produced {
            return Outcome::Ignored;
        }
        self.pending_column = None;
        let column = name.clone();
        self.edit(ctx, move |state| {
            state.fold_key_column = Some(column);
            state.fold_expanded.clear();
            state.fold_enabled = true;
            if state.fold_minimum_run == 0 {
                state.fold_minimum_run = crate::app::DEFAULT_FOLD_MINIMUM_RUN;
            }
        });
        self.message = Some((MessageState::Applied, format!("folding on {name}")));
        Outcome::Consumed
    }

    fn move_choice(&mut self, delta: i32, ctx: &Ctx<'_>) -> Outcome {
        let Some(dropdown) = self.dropdown else {
            return Outcome::Ignored;
        };
        let count = self.choices(dropdown, ctx.views, ctx.provider).0.len();
        if count == 0 {
            return Outcome::Consumed;
        }
        self.highlighted = (self.highlighted as i32 + delta).rem_euclid(count as i32) as usize;
        Outcome::Consumed
    }

    fn key(&mut self, key: KeyEvent, ctx: &mut Ctx<'_>) -> Outcome {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return Outcome::Ignored;
        }
        if self.dropdown.is_some() {
            return match key.code {
                KeyCode::Up | KeyCode::Char('k') => self.move_choice(-1, ctx),
                KeyCode::Down | KeyCode::Char('j') => self.move_choice(1, ctx),
                KeyCode::Char(' ') if self.composing.is_some() => self.toggle_composed(ctx),
                KeyCode::Enter => self.choose(ctx),
                _ => Outcome::Ignored,
            };
        }
        let pattern_key = self.pattern_key(ctx);
        let controls = self.controls(pattern_key);
        let focused = self.focused(ctx);
        match key.code {
            KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.control = Some(move_control(focused, &controls, -1));
                Outcome::Consumed
            }
            KeyCode::BackTab | KeyCode::Up => {
                self.control = Some(move_control(focused, &controls, -1));
                Outcome::Consumed
            }
            KeyCode::Tab | KeyCode::Down => {
                self.control = Some(move_control(focused, &controls, 1));
                Outcome::Consumed
            }
            // §8.2: Space never presses a button, and never executes the
            // default. §8.4/§8.3: it toggles the focused checkbox and opens a
            // focused dropdown, exactly as Enter does there.
            KeyCode::Char(' ') if focused == FoldingControl::Collapse => Outcome::Ignored,
            KeyCode::Char(' ') => self.activate_control(focused, ctx),
            // §8.9: the default button runs the default; every other control
            // here is a checkbox or a closed dropdown, which consume Enter
            // themselves. Both routes read `default_control`, so a control that
            // does not consume Enter cannot end up disagreeing with the fill.
            KeyCode::Enter if focused == Self::default_control() => self.run_default(ctx),
            KeyCode::Enter => self.activate_control(focused, ctx),
            _ => Outcome::Ignored,
        }
    }

    fn mouse(
        &mut self,
        kind: MouseEventKind,
        hit: Option<FoldingHit>,
        ctx: &mut Ctx<'_>,
    ) -> Outcome {
        match kind {
            MouseEventKind::Down(MouseButton::Left) => match hit {
                Some(FoldingHit::Choice(index)) => {
                    self.highlighted = index;
                    if self.composing.is_some() {
                        self.toggle_composed(ctx)
                    } else {
                        self.choose(ctx)
                    }
                }
                Some(FoldingHit::Control(control)) => {
                    // A click outside an open list closes it first, exactly as
                    // Escape does: the innermost surface goes first (§5.3).
                    if self.dropdown.take().is_some() {
                        self.composing = None;
                        self.control = Some(control);
                        return Outcome::Consumed;
                    }
                    self.control = Some(control);
                    self.activate(ctx)
                }
                _ => Outcome::Consumed,
            },
            MouseEventKind::ScrollUp if self.dropdown.is_some() => self.move_choice(-1, ctx),
            MouseEventKind::ScrollDown if self.dropdown.is_some() => self.move_choice(1, ctx),
            _ => Outcome::Ignored,
        }
    }
}

impl Component for FoldingDialog {
    type Hit = FoldingHit;
    type Open = ();

    fn open(&mut self, _params: (), ctx: &mut Ctx<'_>) {
        self.open = true;
        self.dropdown = None;
        self.composing = None;
        self.highlighted = 0;
        self.message = None;
        // A generated column that never landed is not carried into a later
        // visit; only the visit that asked for it may adopt one.
        self.pending_column = None;
        self.geometry = FoldingGeometry::default();
        self.surface = Surface::default();
        // The dialog opens on the choice it is about (§12, default action):
        // the key column, not the toggle that is already visible in the status
        // line. Nothing else about opening touches the view.
        // §12 default action: the dialog opens on the choice it is about — the
        // key column — rather than on the toggle the status line already shows.
        let _ = ctx;
        self.control = Some(FoldingControl::KeyColumn);
    }

    fn handle(&mut self, event: Event<FoldingHit>, ctx: &mut Ctx<'_>) -> Outcome {
        match event {
            Event::Key(key) => self.key(key, ctx),
            Event::Mouse { kind, hit, .. } => self.mouse(kind, hit, ctx),
            // §5.3: the innermost surface closes first, so an open list absorbs
            // Escape and the checkbox list falls back to the ordinary picker.
            Event::Dismiss => {
                if self.composing.take().is_some() {
                    self.highlighted = 0;
                    self.message = None;
                    return Outcome::Consumed;
                }
                if self.dropdown.take().is_some() {
                    return Outcome::Consumed;
                }
                self.open = false;
                self.pending_column = None;
                Outcome::Close
            }
            // §4.2: the shell says what happened to the view. A saved step is
            // how `[ New column… ]` finishes, and this layer decides that for
            // itself from an event fenced on its own view and on the name it
            // asked for.
            Event::View(ViewEvent::QueryAccepted {
                view_id,
                purpose: QueryPurpose::Enrichment,
                ..
            }) => self.adopt_pending(&view_id, ctx),
            Event::Paste(_) | Event::Command(_) | Event::View(_) | Event::Resize => {
                Outcome::Ignored
            }
        }
    }

    fn surface(&self) -> Surface {
        self.surface
    }

    fn hit(&self, point: (u16, u16)) -> Option<FoldingHit> {
        let g = &self.geometry;
        g.choices
            .iter()
            .find_map(|(rect, index)| contains(*rect, point).then_some(FoldingHit::Choice(*index)))
            .or_else(|| {
                g.controls.iter().find_map(|(rect, control)| {
                    contains(*rect, point).then_some(FoldingHit::Control(*control))
                })
            })
            .or_else(|| contains(g.body, point).then_some(FoldingHit::Body))
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx<'_>) -> Surface {
        use crate::dialog_layout::{DialogClass, DialogContent, anchored_rect, content_width};

        let theme = ctx.theme;
        let ascii = ctx.ascii;
        let styles = DialogStyles::new(theme);
        let mut controls_hit: Vec<(Rect, FoldingControl)> = Vec::new();
        let mut choices_hit: Vec<(Rect, usize)> = Vec::new();

        let Some(state) = ctx.views.active() else {
            self.geometry = FoldingGeometry::default();
            self.surface = Surface::default();
            return self.surface;
        };
        let enabled = state.fold_enabled;
        let key_column = state.fold_key_column.clone();
        let pattern_key = key_column.is_none();
        let minimum_run = if state.fold_minimum_run == 0 {
            crate::app::DEFAULT_FOLD_MINIMUM_RUN
        } else {
            state.fold_minimum_run
        };
        let summary = state.fold_summary;
        let controls = self.controls(pattern_key);
        let focused = match self.control {
            Some(control) if controls.contains(&control) => control,
            _ => controls[0],
        };

        // §4.2: one label column, every field starting at the same x.
        let mut rows: Vec<(FoldingControl, &'static str, String)> = vec![
            (
                FoldingControl::Enabled,
                "Fold repeated",
                // §8.4: a checkbox is already ASCII, so it needs no fallback.
                format!("{} on", if enabled { "[x]" } else { "[ ]" }),
            ),
            (
                FoldingControl::KeyColumn,
                "Key column",
                fold_key_label(key_column.as_deref()),
            ),
            (
                FoldingControl::MinimumRun,
                "Minimum run",
                format!("{minimum_run} or more"),
            ),
            (
                FoldingControl::Scope,
                "Scope",
                scope_label(state.fold_lookback),
            ),
        ];
        if pattern_key {
            rows.push((
                FoldingControl::Normalisation,
                "Normalisation",
                state.fold_normalisation.label().to_owned(),
            ));
        }

        // §7.4: one message row, one state word, and an honest sentence about
        // what folding is actually doing right now.
        let (message_state, sentence) = match (&self.message, enabled, summary) {
            (Some((state, sentence)), _, _) => (*state, sentence.clone()),
            (None, false, _) => (
                MessageState::Disabled,
                "every row is listed individually".to_owned(),
            ),
            (None, true, Some(summary)) if summary.enabled => (
                MessageState::Applied,
                format!(
                    "{} on {} · {} run{} collapsed · {} row{} hidden",
                    if summary.folded_entries == 0 {
                        "no runs yet"
                    } else {
                        "folding"
                    },
                    fold_key_label(key_column.as_deref()),
                    summary.folded_entries,
                    if summary.folded_entries == 1 { "" } else { "s" },
                    summary.hidden_rows,
                    if summary.hidden_rows == 1 { "" } else { "s" },
                ),
            ),
            (None, true, _) => (
                MessageState::Updating,
                format!("folding on {}", fold_key_label(key_column.as_deref())),
            ),
        };
        let help = if pattern_key {
            "The pattern column is the row text with timestamps, ids and numbers replaced. \
             Any other column folds on its value unchanged."
        } else {
            "This column's value is the key as it stands: nothing is normalised, and no other \
             field is replaced."
        };
        let action_controls = [FoldingControl::Collapse];
        let action_labels = ["Collapse expanded runs"];
        let width = content_width(area, DialogClass::M);
        let content = DialogContent {
            header: 0,
            body: u16::try_from(rows.len()).unwrap_or(4),
            message: message_rows(&sentence, width),
            help: help_rows(help, width),
            actions: packed_button_rows(width, &action_labels),
        };
        let title = match ctx.views.active_item() {
            Some(view) => format!("Folding · {}", view.name),
            None => "Folding".to_owned(),
        };
        let regions =
            crate::ui::dialog_frame_regions(frame, area, DialogClass::M, &title, &content, theme);
        let mut surface = Surface {
            popup: regions.popup,
            interior: regions.interior,
            caret: None,
            // Nothing here scrolls except an open list, and the wheel over the
            // dialog itself must not reach the log behind it either way.
            scrollable: self.dropdown.is_some(),
            text_focus: false,
        };
        self.geometry = FoldingGeometry {
            body: regions.body,
            ..FoldingGeometry::default()
        };
        self.surface = surface;
        if regions.content.width == 0 {
            return surface;
        }

        let label_width = rows
            .iter()
            .map(|(_, label, _)| u16::try_from(UnicodeWidthStr::width(*label)).unwrap_or(0))
            .max()
            .unwrap_or(0)
            .min(18);
        let field_x = regions
            .content
            .x
            .saturating_add(label_width)
            .saturating_add(FIELD_GUTTER);
        for (offset, (control, label, value)) in rows.iter().enumerate() {
            let y = regions
                .body
                .y
                .saturating_add(u16::try_from(offset).unwrap_or(0));
            if y >= regions.body.bottom() {
                break;
            }
            let is_focused = *control == focused;
            frame.render_widget(
                Paragraph::new(*label).style(if is_focused {
                    styles.shortcut
                } else {
                    styles.label
                }),
                Rect::new(
                    regions.content.x,
                    y,
                    label_width.min(regions.content.width),
                    1,
                ),
            );
            if field_x >= regions.content.right() {
                continue;
            }
            let field = Rect::new(
                field_x,
                y,
                regions.content.right().saturating_sub(field_x),
                1,
            );
            if *control == FoldingControl::Enabled {
                // §8.4: a checkbox is text, not an input surface.
                let mark = if ascii && enabled {
                    "[x] on"
                } else {
                    value.as_str()
                };
                frame.render_widget(
                    Paragraph::new(truncated(mark, usize::from(field.width))).style(
                        if is_focused {
                            styles.shortcut
                        } else {
                            styles.label
                        },
                    ),
                    field,
                );
                controls_hit.push((field, *control));
                continue;
            }
            // §8.3: a dropdown is a field with a chevron in its last cell.
            let style = if is_focused {
                styles.selection
            } else {
                styles.input
            };
            InputSurface { style }.render(field, frame.buffer_mut());
            frame.render_widget(
                Paragraph::new(truncated(value, usize::from(field.width.saturating_sub(2))))
                    .style(style),
                field,
            );
            frame.render_widget(
                Paragraph::new(if ascii { "v" } else { "▾" }).style(
                    Style::default().fg(theme.accent).bg(if is_focused {
                        theme.selection_bg
                    } else {
                        theme.input_bg
                    }),
                ),
                Rect::new(field.right().saturating_sub(1), field.y, 1, 1),
            );
            controls_hit.push((field, *control));
        }

        render_message(
            frame,
            regions.message,
            message_state,
            &sentence,
            theme,
            ascii,
        );
        render_help_text(frame, regions.help, help, theme);
        // §8.2/§8.9: the row is declared, and the one verb in it is the
        // default, so it carries the fill.
        let default_action = action_controls
            .iter()
            .position(|control| *control == Self::default_control());
        for (index, rect) in render_actions(
            frame,
            regions.actions,
            ActionRow {
                labels: &action_labels,
                default: default_action,
                destructive: &[],
                focused: action_controls
                    .iter()
                    .position(|control| *control == focused),
            },
            theme,
        ) {
            controls_hit.push((rect, action_controls[index]));
        }

        if let Some(dropdown) = self.dropdown {
            let (choices, _) = self.choices(dropdown, ctx.views, ctx.provider);
            let anchor = controls_hit
                .iter()
                .find(|(_, control)| {
                    *control
                        == match dropdown {
                            FoldingDropdown::KeyColumn => FoldingControl::KeyColumn,
                            FoldingDropdown::MinimumRun => FoldingControl::MinimumRun,
                            FoldingDropdown::Scope => FoldingControl::Scope,
                            FoldingDropdown::Normalisation => FoldingControl::Normalisation,
                        }
                })
                .map_or(regions.body, |(rect, _)| *rect);
            // §5.2.1: the reservation and the width both come from the frame
            // and the field, never from the choices. The key-column list is fed
            // by a bounded sample of the view's rows, so a column that appears
            // while the list is open — or a checkmark added in compose mode —
            // must not resize the popup or move the rows under the cursor.
            // §10: an anchored popup is not a dialog; it may extend past the
            // dialog it belongs to and is bounded by the frame.
            let reserved = picker_rows(area);
            let box_area = anchored_rect(area, anchor, reserved, 0);
            if box_area.width >= 3 && box_area.height >= 3 {
                surface.popup = surface.popup.union(box_area);
                frame.render_widget(Clear, box_area);
                let rows = usize::from(box_area.height.saturating_sub(2));
                let cell_width = usize::from(box_area.width.saturating_sub(2));
                // §5.2.1 overflow: the last reserved row says how much is not
                // shown, because a bare popup has no §8.7 heading to carry a
                // count.
                let overflowing = choices.len() > rows;
                let visible = if overflowing {
                    rows.saturating_sub(1)
                } else {
                    rows
                };
                let selected = self.highlighted.min(choices.len().saturating_sub(1));
                let scroll = selected
                    .saturating_add(1)
                    .saturating_sub(visible)
                    .min(choices.len().saturating_sub(visible.min(choices.len())));
                let mut items: Vec<ListItem> = choices
                    .iter()
                    .enumerate()
                    .skip(scroll)
                    .take(visible)
                    .map(|(index, value)| {
                        ListItem::new(Line::from(truncated(value, cell_width))).style(
                            if index == selected {
                                styles.selection
                            } else {
                                styles.label
                            },
                        )
                    })
                    .collect();
                if overflowing {
                    let hidden = choices.len().saturating_sub(visible);
                    items.push(
                        ListItem::new(Line::from(truncated(
                            &format!("+{hidden} more"),
                            cell_width,
                        )))
                        .style(styles.description),
                    );
                }
                frame.render_widget(
                    List::new(items).block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(theme.active_border)),
                    ),
                    box_area,
                );
                for (offset, index) in (scroll..choices.len()).take(visible).enumerate() {
                    choices_hit.push((
                        Rect::new(
                            box_area.x.saturating_add(1),
                            box_area.y.saturating_add(1).saturating_add(offset as u16),
                            box_area.width.saturating_sub(2),
                            1,
                        ),
                        index,
                    ));
                }
            }
        }

        self.geometry = FoldingGeometry {
            body: regions.body,
            controls: controls_hit,
            choices: choices_hit,
        };
        self.surface = surface;
        surface
    }
}
