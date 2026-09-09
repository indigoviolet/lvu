//! The Fields layer (`docs/dialog-system.md` §12.11), converted per
//! `docs/component-model.md` §6.3 step 3, and since §8.11–§8.12 the place a
//! record's structure and a field's values are explored.
//!
//! Fields reads the provider and writes the active view: it pins columns,
//! colours rows by a field, folds by a field, filters to or excludes a value,
//! and hands the record off to Raw context or to the cross-source
//! correlation lookup. Which row is selected, which control has focus, which
//! record is anchored and which paths are open stay in `ViewState` — §7.3
//! records those as accepted debt, because they persist per view across open
//! and close. The list's scroll offset and the statistics cache do not: the
//! first is geometry and the second is derived from a provider revision, so
//! both live here.

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::{
    Frame,
    layout::Rect,
    style::Modifier,
    text::{Line, Span},
    widgets::Paragraph,
};
use unicode_width::UnicodeWidthStr;

use crate::app::{Action, FieldPickerControl, QueryPurpose, Views, python_string_literal};
use crate::command_palette::CommandId;
use crate::component::{
    CommandEntry, CommandSpec, Component, Ctx, Event, Outcome, RenderCtx, Surface,
};
use crate::details::{disclosure, json_kind_style};
use crate::dialog_controls::{ActionRow, DialogStyles};
use crate::field_stats::{FieldStats, MAX_STATS_ROWS, ValueType, field_stats};
use crate::json_spans::JsonKind;
use crate::json_tree::{JsonTree, RowShape, json_path, nested_suffix, top_level_key};
use crate::provider::{DisplayRow, RowId, RowProvider};
use crate::text_edit::TextTarget;
use crate::theme::Theme;
use crate::ui::{
    FIELD_GUTTER, MessageState, dialog_frame_regions, help_rows, message_rows, packed_button_rows,
    render_actions, render_help_text, render_message, render_scrollbar, truncated,
};

/// §12.11: the name column, wide enough for the field names a record carries
/// without pushing the value off the row.
const FIELD_NAME_WIDTH: u16 = 14;
/// `[ ] ` — the §8.4 checkbox and its trailing space.
const FIELD_CHECKBOX_WIDTH: u16 = 4;
/// §8.12: the Value pane is a fixed-height region, so moving the selection
/// never resizes the dialog (§5.2.1: content that changes with a keystroke
/// is reserved, not measured).
pub const VALUE_PANE_LINES: u16 = 8;
/// Two panes side by side need this much content width; below it they stack.
const SIDE_BY_SIDE_WIDTH: u16 = 72;

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

/// §8.12 statistics, computed once per (view, provider revision, path) and
/// read every frame. Derived data, not rows (§7.10).
#[derive(Clone, Debug)]
struct StatsCache {
    view_id: String,
    revision: u64,
    stats: FieldStats,
}

#[derive(Debug, Default)]
pub struct FieldsDialog {
    open: bool,
    /// First visible row of the field list.
    top: usize,
    geometry: FieldsGeometry,
    surface: Surface,
    stats: Option<StatsCache>,
}

/// One row of the field list: a tree row for a JSON record, a flat
/// recognised field otherwise (§8.11).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FieldRow {
    /// `a.b[2].c`; the key of the expansion memory and of the actions.
    pub path: String,
    pub label: String,
    pub depth: usize,
    pub shape: FieldShape,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FieldShape {
    Container {
        expanded: bool,
        summary: String,
    },
    /// The value as the record spelled it, and its JSON kind when known.
    Scalar {
        text: String,
        kind: Option<JsonKind>,
    },
}

impl FieldRow {
    pub fn is_container(&self) -> bool {
        matches!(self.shape, FieldShape::Container { .. })
    }
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

/// The rows the list shows for `row`, given which paths are open.
pub fn field_rows(
    row: &DisplayRow,
    expanded: &std::collections::BTreeSet<String>,
) -> Vec<FieldRow> {
    if let Some(tree) = JsonTree::parse(&row.text).filter(JsonTree::is_object) {
        return tree
            .rows(&|path| expanded.contains(path))
            .into_iter()
            .map(|tree_row| FieldRow {
                shape: match tree_row.shape {
                    RowShape::Container { expanded, .. } => FieldShape::Container {
                        expanded,
                        summary: tree.summary(tree_row.node).unwrap_or_default(),
                    },
                    RowShape::Scalar(kind) => FieldShape::Scalar {
                        text: tree
                            .scalar_text(&row.text, tree_row.node)
                            .unwrap_or_default()
                            .to_owned(),
                        kind: Some(kind),
                    },
                },
                path: tree_row.path,
                label: tree_row.label,
                depth: tree_row.depth,
            })
            .collect();
    }
    row.fields
        .iter()
        .map(|(key, value)| FieldRow {
            path: key.clone(),
            label: key.clone(),
            depth: 0,
            shape: FieldShape::Scalar {
                text: value.clone(),
                kind: None,
            },
        })
        .collect()
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
        description: "Toggle the selected field as a log column",
        category: "Fields",
        aliases: &["column", "visible"],
        shortcut: None,
    },
    CommandSpec {
        id: CommandId::FilterToFieldValue,
        name: "Filter to this value",
        description: "Keep only records whose field has the selected value",
        category: "Fields",
        aliases: &["only", "equals", "keep"],
        shortcut: None,
    },
    CommandSpec {
        id: CommandId::ExcludeFieldValue,
        name: "Exclude this value",
        description: "Drop records whose field has the selected value",
        category: "Fields",
        aliases: &["not", "hide", "without"],
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
        id: CommandId::FoldByField,
        name: "Fold or unfold by this field",
        description: "Toggle collapsing runs of records that share this field's value",
        category: "Fields",
        aliases: &["group runs", "collapse", "unfold"],
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

/// The key that reaches each command while the layer is on top: the letter
/// each button underlines (§8.10). Fields takes no text, so the bare letter is
/// always live here and it is what the palette prints.
fn fields_command_shortcut(id: CommandId) -> Option<&'static str> {
    match id {
        CommandId::PinField => Some("Space"),
        CommandId::FilterToFieldValue => Some("f"),
        CommandId::ExcludeFieldValue => Some("x"),
        CommandId::ColorField => Some("c"),
        CommandId::FoldByField => Some("d"),
        CommandId::CorrelateField => Some("r"),
        _ => None,
    }
}

/// §8.9/§8.10: the action row, computed from the shared state that `render`
/// and the key handler can both see. One function, so the letter drawn with an
/// underline and the letter that presses the button cannot disagree.
///
/// With no fields but a record there is still something to inspect.
fn action_buttons(
    views: &Views,
    provider: &dyn RowProvider,
) -> Vec<(&'static str, FieldPickerControl)> {
    use FieldPickerControl as C;
    let has_anchor = anchor_id(views).is_some();
    let expanded = views
        .active()
        .map(|state| state.expanded_paths.clone())
        .unwrap_or_default();
    let fields: Vec<FieldRow> = anchored_row(views, provider)
        .as_ref()
        .map_or_else(Vec::new, |row| field_rows(row, &expanded));
    if fields.is_empty() {
        // Nothing to pin, but the record itself is still inspectable.
        return if has_anchor {
            vec![("Raw c&ontext", C::Context)]
        } else {
            Vec::new()
        };
    }
    let selected = views
        .active()
        .map_or(0, |state| state.field_picker_selected)
        .min(fields.len().saturating_sub(1));
    let selected_column = fields
        .get(selected)
        .map(|row| top_level_key(&row.path).to_owned());
    let pinned = views
        .active()
        .map_or_else(Vec::new, |state| state.pinned_columns.clone());
    let color_field = views.active().and_then(|state| state.color_field.clone());
    // The column this view is run-grouped by right now, if it is. A one-key
    // action that only ever turns something on leaves the user looking for
    // where to turn it off; §8.9's Add/Edit rule is that the button says what
    // pressing it will do from here.
    let folded_by = views.active().and_then(|state| {
        match crate::grouping::parse_grouping(&state.grouping.applied) {
            Ok(crate::grouping::GroupingSpec::Run { column }) => Some(column.to_owned()),
            _ => None,
        }
    });
    let pin_label = if selected_column
        .as_ref()
        .is_some_and(|key| pinned.contains(key))
    {
        "&Unpin"
    } else {
        "&Pin"
    };
    let color_label = if selected_column
        .as_deref()
        .is_some_and(|key| color_field.as_deref() == Some(key))
    {
        "Stop &colouring"
    } else {
        "&Color"
    };
    let (severity_role, timestamp_role) = views
        .active()
        .map(|state| {
            (
                state.severity_column.clone(),
                state.timestamp_column.clone(),
            )
        })
        .unwrap_or_default();
    let severity_label = if selected_column
        .as_deref()
        .is_some_and(|key| severity_role.as_deref() == Some(key))
    {
        "Stop &severity"
    } else {
        "&Severity"
    };
    let timestamp_label = if selected_column
        .as_deref()
        .is_some_and(|key| timestamp_role.as_deref() == Some(key))
    {
        "Stop &timestamp"
    } else {
        "&Timestamp"
    };
    let fold_label = if selected_column
        .as_deref()
        .is_some_and(|key| folded_by.as_deref() == Some(key))
    {
        "Unfol&d"
    } else {
        "Fol&d"
    };
    vec![
        (pin_label, C::Pin),
        ("&Filter", C::Filter),
        ("E&xclude", C::Exclude),
        (color_label, C::Color),
        (severity_label, C::Severity),
        (timestamp_label, C::Timestamp),
        (fold_label, C::Fold),
        ("Co&rrelate", C::Correlate),
    ]
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

    /// The statistics the Value pane last showed, for tests.
    pub fn stats(&self) -> Option<&FieldStats> {
        self.stats.as_ref().map(|cache| &cache.stats)
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

    /// The rows as the list shows them right now.
    fn rows(ctx: &Ctx<'_>) -> Vec<FieldRow> {
        let Some(row) = anchored_row(ctx.views, ctx.provider) else {
            return Vec::new();
        };
        let expanded = ctx
            .views
            .active()
            .map(|state| state.expanded_paths.clone())
            .unwrap_or_default();
        field_rows(&row, &expanded)
    }

    /// The row the actions act on, resolved through the provider every time
    /// rather than remembered (§7.10).
    fn selected_row(ctx: &Ctx<'_>) -> Option<FieldRow> {
        let rows = Self::rows(ctx);
        let selected = ctx
            .views
            .active()
            .map_or(0, |state| state.field_picker_selected);
        rows.get(selected.min(rows.len().saturating_sub(1)))
            .cloned()
    }

    /// The column a path acts through: itself at the top level, its
    /// top-level ancestor below, because nested values are JSON text inside
    /// that column on the query side (§8.12).
    fn selected_column(ctx: &Ctx<'_>) -> Option<String> {
        Self::selected_row(ctx).map(|row| top_level_key(&row.path).to_owned())
    }

    fn move_selection(&mut self, delta: i32, ctx: &mut Ctx<'_>) {
        let count = Self::rows(ctx).len();
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
            FieldPickerControl::Filter,
            FieldPickerControl::Exclude,
            FieldPickerControl::Color,
            FieldPickerControl::Severity,
            FieldPickerControl::Timestamp,
            FieldPickerControl::Fold,
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

    /// Enter (§8.9): a container row consumes it to open or close; every
    /// other row hands it to the default, `Pin`; a button presses itself.
    fn activate(&mut self, ctx: &mut Ctx<'_>) -> Outcome {
        match Self::control(ctx) {
            FieldPickerControl::List => {
                if Self::selected_row(ctx).is_some_and(|row| row.is_container()) {
                    self.set_expanded(None, ctx)
                } else {
                    self.toggle_field(true, ctx)
                }
            }
            FieldPickerControl::Pin => self.toggle_field(true, ctx),
            FieldPickerControl::Color => self.toggle_field(false, ctx),
            FieldPickerControl::Severity => self.toggle_role(true, ctx),
            FieldPickerControl::Timestamp => self.toggle_role(false, ctx),
            FieldPickerControl::Filter => self.filter_to_value(false, ctx),
            FieldPickerControl::Exclude => self.filter_to_value(true, ctx),
            FieldPickerControl::Fold => self.fold_by_field(ctx),
            FieldPickerControl::Correlate => self.correlate(ctx),
            FieldPickerControl::Context => self.open_context(ctx),
        }
    }

    /// §8.11: open, close or toggle the selected container; Left on a leaf
    /// climbs to the container it sits in.
    fn set_expanded(&mut self, expand: Option<bool>, ctx: &mut Ctx<'_>) -> Outcome {
        let rows = Self::rows(ctx);
        let Some(state) = ctx.views.active_mut() else {
            return Outcome::Consumed;
        };
        let selected = state
            .field_picker_selected
            .min(rows.len().saturating_sub(1));
        let Some(row) = rows.get(selected) else {
            return Outcome::Consumed;
        };
        match (&row.shape, expand) {
            (FieldShape::Container { expanded, .. }, want) => {
                let open = want.unwrap_or(!expanded);
                if open {
                    if state.expanded_paths.len() < crate::app::MAX_EXPANDED_PATHS
                        || state.expanded_paths.contains(&row.path)
                    {
                        state.expanded_paths.insert(row.path.clone());
                    }
                } else {
                    state.expanded_paths.remove(&row.path);
                }
            }
            (FieldShape::Scalar { .. }, Some(false)) => {
                if let Some(parent) = rows[..selected]
                    .iter()
                    .rposition(|candidate| candidate.depth + 1 == row.depth)
                {
                    state.field_picker_selected = parent;
                }
            }
            _ => {}
        }
        Outcome::Consumed
    }

    /// Pin/unpin, or colour/stop colouring, the selected field's column. Both
    /// write the active view and nothing else, so the log re-renders from
    /// `Views` (§4.2).
    fn toggle_field(&mut self, pin: bool, ctx: &mut Ctx<'_>) -> Outcome {
        let Some(field) = Self::selected_column(ctx) else {
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

    /// Whether the anchored record proves an accepted enrichment evaluated
    /// `field`: the same `derived.{name}` marker rendering resolves roles
    /// through, read here so assignment and consumption cannot disagree.
    fn role_proven(ctx: &Ctx<'_>, field: &str) -> bool {
        anchored_row(ctx.views, ctx.provider).is_some_and(|row| {
            row.details
                .iter()
                .any(|(key, _)| key == &format!("derived.{field}"))
        })
    }

    /// Name the selected column for a display role, or stop using it when it
    /// is already the role. Assigning requires the anchored record's
    /// `derived.{name}` marker — proof the accepted chain evaluated it — so
    /// a raw same-name field can never be assigned: rendering resolves roles
    /// through the same marker, and the refusal says why instead of silently
    /// doing nothing. Naming a timestamp role also stages the authoritative
    /// Selected time basis for that column through the normal Time candidate
    /// fences, so the gutter never becomes a second independent time
    /// selector: the Time dialog still reviews and applies. Clearing the role
    /// leaves an explicitly chosen basis alone.
    fn toggle_role(&mut self, severity: bool, ctx: &mut Ctx<'_>) -> Outcome {
        let Some(field) = Self::selected_column(ctx) else {
            return Outcome::Consumed;
        };
        if !Self::role_proven(ctx, &field) {
            ctx.notice(format!(
                "only an accepted enrichment output can feed a role; {field:?} has none"
            ));
            return Outcome::Consumed;
        }
        let Some(state) = ctx.views.active_mut() else {
            return Outcome::Consumed;
        };
        if severity {
            let role = &mut state.severity_column;
            if role.as_deref() == Some(&field) {
                *role = None;
            } else {
                *role = Some(field);
            }
        } else if state.timestamp_column.as_deref() == Some(&field) {
            state.timestamp_column = None;
        } else {
            crate::app::stage_timestamp_basis(state, &field);
            state.timestamp_column = Some(field);
        }
        state.user_interaction_revision = state.user_interaction_revision.saturating_add(1);
        Outcome::Consumed
    }

    /// §8.12: run-group the view by the selected field's column through the
    /// unified grouping control — and, when the view is already run-grouped
    /// by it, clear the rule again.
    ///
    /// §8.9's rule for Add/Edit: a one-key action follows the state it acts on
    /// rather than only ever switching it on. Recognition lives in
    /// Enrichment: the column must be an accepted enrichment output, and the
    /// worker names any other column actionably while the last-good view
    /// stays put.
    fn fold_by_field(&mut self, ctx: &mut Ctx<'_>) -> Outcome {
        let Some(column) = Self::selected_column(ctx) else {
            return Outcome::Consumed;
        };
        let Some(view_id) = ctx.views.active_id().map(str::to_owned) else {
            return Outcome::Consumed;
        };
        let grouped_by_this = ctx.views.active().is_some_and(|state| {
            matches!(
                crate::grouping::parse_grouping(&state.grouping.applied),
                Ok(crate::grouping::GroupingSpec::Run { column: applied })
                    if applied == column.as_str()
            )
        });
        let rule = if grouped_by_this {
            String::new()
        } else {
            crate::grouping::run_rule(&column)
        };
        if ctx
            .views
            .enqueue(&view_id, QueryPurpose::Grouping, Some(rule))
            .is_err()
        {
            ctx.notice("query submission queue is full; draft was preserved".to_owned());
            return Outcome::Consumed;
        }
        ctx.views.touch(&view_id);
        ctx.notice(if grouped_by_this {
            format!("grouping off; runs were on {column}")
        } else {
            format!("grouping runs on {column}")
        });
        Outcome::Consumed
    }

    /// §8.12: keep (or drop) the records whose field has the selected value.
    /// The predicate becomes the Advanced filter, joined to the one already
    /// applied, so the user can see and edit exactly what was submitted.
    fn filter_to_value(&mut self, exclude: bool, ctx: &mut Ctx<'_>) -> Outcome {
        let Some(row) = Self::selected_row(ctx) else {
            return Outcome::Consumed;
        };
        let FieldShape::Scalar { text, kind } = &row.shape else {
            ctx.notice("select a value, not an object or array");
            return Outcome::Consumed;
        };
        let Some(predicate) = value_predicate(&row.path, text, kind.as_ref(), exclude) else {
            ctx.notice("this value cannot be turned into a filter");
            return Outcome::Consumed;
        };
        let Some(view_id) = ctx.views.active_id().map(str::to_owned) else {
            return Outcome::Consumed;
        };
        let applied = ctx
            .views
            .editor(&view_id, QueryPurpose::Advanced)
            .map(|editor| editor.applied.trim().to_owned())
            .unwrap_or_default();
        let expression = if applied.is_empty() {
            predicate
        } else {
            format!("({applied}) & ({predicate})")
        };
        if let Some(editor) = ctx.views.editor_mut(&view_id, QueryPurpose::Advanced) {
            editor.draft = expression.clone();
            editor.error = None;
        }
        ctx.cursors.reset(
            TextTarget {
                identity: view_id.clone(),
                field: "advanced",
            },
            &expression,
        );
        match ctx.views.enqueue(&view_id, QueryPurpose::Advanced, None) {
            Ok(_) => ctx.notice(if exclude {
                format!("excluding {} = {}", row.path, text)
            } else {
                format!("filtering to {} = {}", row.path, text)
            }),
            Err(refused) => ctx.notice(format!("filter not submitted: {refused:?}")),
        }
        Outcome::Consumed
    }

    /// Correlation is its own layer (`components/correlation.rs`): this one
    /// hands it the frozen record and the field and is replaced by it, so the
    /// lookup's pending state is shown where the answer will land.
    fn correlate(&mut self, ctx: &mut Ctx<'_>) -> Outcome {
        let Some(row) = anchored_row(ctx.views, ctx.provider) else {
            ctx.notice("field data is pending or unavailable");
            return Outcome::Consumed;
        };
        let Some(field) = Self::selected_column(ctx) else {
            ctx.notice("field data is pending or unavailable");
            return Outcome::Consumed;
        };
        self.open = false;
        Outcome::Replace(crate::component::Open::Correlation(
            crate::components::correlation::CorrelationOpen { row: row.id, field },
        ))
    }

    /// Raw context is a jump to the record in All events
    /// (raw-context-as-jump.md): this dialog closes and is re-pushed on
    /// return, opening on the anchor the view remembers.
    fn open_context(&mut self, ctx: &mut Ctx<'_>) -> Outcome {
        let Some(anchor) = anchor_id(ctx.views).cloned() else {
            return Outcome::Consumed;
        };
        self.open = false;
        Outcome::Legacy(Action::RawContext {
            anchor: Some(anchor),
            layer: Some(crate::component::Open::Fields),
        })
    }

    fn key(&mut self, key: KeyEvent, ctx: &mut Ctx<'_>) -> Outcome {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return Outcome::Ignored;
        }
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Down | KeyCode::Char('j') if !alt => self.move_selection(1, ctx),
            KeyCode::Up | KeyCode::Char('k') if !alt => self.move_selection(-1, ctx),
            KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.move_control(-1, ctx)
            }
            KeyCode::BackTab => self.move_control(-1, ctx),
            KeyCode::Tab => self.move_control(1, ctx),
            // §8.11: Right opens, Left closes (or climbs), whichever control
            // has the focus ring, because the list is what they act on.
            KeyCode::Right => return self.set_expanded(Some(true), ctx),
            KeyCode::Left => return self.set_expanded(Some(false), ctx),
            // Space always pins, wherever focus sits (§8.4); Enter activates
            // whichever control has it. Every other letter this dialog answers
            // to is a §8.10 mnemonic on its action row, resolved by the shell
            // in `dispatch_raw` before the key reaches here — the hand-written
            // `c`/`r`/`o` and Alt-`p`/`f`/`x`/`d` arms that used to live here
            // are what made three of the six underlined letters dead.
            KeyCode::Char(' ') => return self.toggle_field(true, ctx),
            KeyCode::Enter => return self.activate(ctx),
            // `o` is the base screen's raw-context key, doing the same thing
            // here: since W21 it is the jump to All events and back, not a
            // dialog. It is a §8.10 mnemonic only in the empty state, where
            // `Raw c&ontext` is the whole row; with fields present no button
            // carries the letter, so this is the unlisted alias §8.10 allows
            // rather than a second spelling of a mnemonic.
            KeyCode::Char('o') if !alt => return self.open_context(ctx),
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
        if matches!(kind, MouseEventKind::Down(MouseButton::Left)) {
            match hit {
                Some(FieldsHit::Row(index)) => {
                    let already = ctx
                        .views
                        .active()
                        .is_some_and(|state| state.field_picker_selected == index);
                    if let Some(state) = ctx.views.active_mut() {
                        state.field_picker_selected = index;
                        state.field_picker_control = FieldPickerControl::List;
                    }
                    // §8.5: a second click on the selected row activates it,
                    // which for a container is the disclosure.
                    if already && Self::selected_row(ctx).is_some_and(|row| row.is_container()) {
                        return self.set_expanded(None, ctx);
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

    /// The Value pane's statistics for `path`, from the cache when the view
    /// and its provider revision are unchanged.
    fn stats_for(
        &mut self,
        view_id: &str,
        revision: u64,
        path: &str,
        provider: &dyn RowProvider,
    ) -> FieldStats {
        if let Some(cache) = &self.stats
            && cache.view_id == view_id
            && cache.revision == revision
            && cache.stats.path == path
        {
            return cache.stats.clone();
        }
        let stats = field_stats(provider, view_id, path);
        self.stats = Some(StatsCache {
            view_id: view_id.to_owned(),
            revision,
            stats: stats.clone(),
        });
        stats
    }
}

/// §8.12: the Advanced predicate for "field has this value". Top-level
/// values compare typed; nested values are addressed by JSON path inside
/// their top-level column, because that column holds them as JSON text, and
/// compared typed too. Only a key the path syntax cannot spell falls back
/// to the lexical pair match.
pub fn value_predicate(
    path: &str,
    text: &str,
    kind: Option<&JsonKind>,
    exclude: bool,
) -> Option<String> {
    let column = python_string_literal(top_level_key(path));
    let predicate = match nested_suffix(path) {
        None => {
            let literal = match kind {
                Some(JsonKind::String) => {
                    python_string_literal(&crate::json_tree::decode_string(text)?)
                }
                Some(JsonKind::Number) => text.to_owned(),
                Some(JsonKind::Boolean) => if text == "true" { "True" } else { "False" }.to_owned(),
                Some(JsonKind::Null) => {
                    return Some(if exclude {
                        format!("pl.col({column}).is_not_null()")
                    } else {
                        format!("pl.col({column}).is_null()")
                    });
                }
                Some(JsonKind::Key(_) | JsonKind::Punctuation) => return None,
                // A recognised (logfmt) field: the column is a string.
                None => python_string_literal(text),
            };
            let operator = if exclude { "!=" } else { "==" };
            return Some(format!("pl.col({column}) {operator} {literal}"));
        }
        Some(_) if json_path(path).is_some() => {
            let matched = format!(
                "pl.col({column}).str.json_path_match({})",
                python_string_literal(&json_path(path)?)
            );
            let operator = if exclude { "!=" } else { "==" };
            // `json_path_match` yields the value as text: a string without
            // its quotes, anything else as spelled. Numbers compare as
            // numbers so `503` is not `5033` and `1.0` is `1`.
            return Some(match kind {
                Some(JsonKind::Number) => {
                    format!("{matched}.cast(pl.Float64, strict=False) {operator} {text}")
                }
                Some(JsonKind::String) => format!(
                    "{matched} {operator} {}",
                    python_string_literal(&crate::json_tree::decode_string(text)?)
                ),
                _ => format!("{matched} {operator} {}", python_string_literal(text)),
            });
        }
        Some(suffix) => {
            let leaf = suffix
                .rsplit(['.', '[', ']'])
                .find(|part| !part.is_empty())?
                .trim_start_matches('[');
            // `"leaf"\s*:\s*<bytes>` — the record's own spelling of the pair,
            // with whitespace allowed around the colon. An array item has no
            // key, so its bytes alone are the needle.
            let needle = if leaf.parse::<usize>().is_ok() {
                regex_escape(text)
            } else {
                format!("\"{}\"\\s*:\\s*{}", regex_escape(leaf), regex_escape(text))
            };
            format!(
                "pl.col({column}).str.contains({})",
                python_string_literal(&needle)
            )
        }
    };
    Some(if exclude {
        format!("~({predicate})")
    } else {
        predicate
    })
}

fn regex_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if r"\.+*?()|[]{}^$".contains(ch) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// `2048` → `2,048`, so a count reads at a glance.
pub fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// The Value pane's lines (§8.12), always [`VALUE_PANE_LINES`] of them so the
/// pane never changes height.
/// The Value pane's body.
///
/// `whole` replaces the sample's *counts* when a whole-view pass has answered
/// for this field, and only its counts: the type line still comes from the
/// sample, because naming a value's type is the app's job and the engine only
/// counted rows under the name the app had already chosen. Keeping the two
/// sources apart here is what stops the pane saying a field is an integer in
/// one line and reporting a different field's arithmetic in the next.
pub fn value_pane_lines(
    stats: Option<&FieldStats>,
    whole: Option<&crate::app::WholeViewStats>,
    row: Option<&FieldRow>,
    theme: Theme,
) -> Vec<Line<'static>> {
    let styles = DialogStyles::new(theme);
    let label = |text: &str| Span::styled(format!("{text:<10}"), styles.label);
    let value = |text: String| Span::styled(text, styles.description);
    let mut lines: Vec<Line<'static>> = Vec::new();
    match (stats, row) {
        (Some(stats), Some(row)) => {
            match &stats.guess {
                Some(guess) => {
                    lines.push(Line::from(vec![
                        label("Type"),
                        value(format!(
                            "{} · {}% of present values",
                            guess.kind.label(),
                            guess.confidence_percent(stats.present)
                        )),
                    ]));
                    lines.push(Line::from(vec![
                        label("Sample"),
                        value(format!(
                            "{} · record {}",
                            guess.sample, guess.sample_row.sequence
                        )),
                    ]));
                }
                None => {
                    lines.push(Line::from(vec![
                        label("Type"),
                        value("no values in the sample".into()),
                    ]));
                    lines.push(Line::from(vec![label("Sample"), value("—".into())]));
                }
            }
            let (present, of_records, distinct, distinct_capped) = match whole {
                Some(whole) => (
                    whole.present as usize,
                    whole.records as usize,
                    whole.distinct as usize,
                    whole.distinct_capped,
                ),
                None => (
                    stats.present,
                    stats.sampled,
                    stats.distinct,
                    stats.distinct_capped,
                ),
            };
            lines.push(Line::from(vec![
                label("Present"),
                value(format!(
                    "{} of {} {}records",
                    thousands(present),
                    thousands(of_records),
                    if whole.is_some() { "" } else { "sampled " }
                )),
            ]));
            lines.push(Line::from(vec![
                label("Distinct"),
                value(if distinct_capped {
                    format!("{}+ values", thousands(distinct))
                } else {
                    format!(
                        "{} value{}",
                        thousands(distinct),
                        if distinct == 1 { "" } else { "s" }
                    )
                }),
            ]));
            let range = match whole {
                Some(whole) => whole.minimum.clone().zip(whole.maximum.clone()),
                None => stats.range.clone(),
            };
            lines.push(Line::from(vec![
                label("Range"),
                value(
                    match (&range, stats.guess.as_ref().map(|guess| guess.kind)) {
                        (Some((min, max)), _) => format!("{min} … {max}"),
                        (
                            None,
                            Some(ValueType::Integer | ValueType::Float | ValueType::Timestamp),
                        ) => "—".into(),
                        _ => "not numeric".into(),
                    },
                ),
            ]));
            // The two sources count in different widths; the pane shows the
            // same thing either way.
            let top: Vec<(String, u64)> = match whole {
                Some(whole) => whole.top.clone(),
                None => stats
                    .top
                    .iter()
                    .map(|(text, count)| (text.clone(), *count as u64))
                    .collect(),
            };
            for (index, (text, count)) in top.iter().take(3).enumerate() {
                lines.push(Line::from(vec![
                    label(if index == 0 { "Top" } else { "" }),
                    Span::styled(
                        format!(
                            "{:>6}  ",
                            thousands(usize::try_from(*count).unwrap_or(usize::MAX))
                        ),
                        styles.description,
                    ),
                    value(text.clone()),
                ]));
            }
            if row.is_container() {
                lines.push(Line::styled(
                    "an object or array · open it to explore its values",
                    styles.unavailable,
                ));
            }
        }
        _ => lines.push(Line::styled(
            "select a field to explore its values",
            styles.unavailable,
        )),
    }
    lines.truncate(usize::from(VALUE_PANE_LINES));
    while lines.len() < usize::from(VALUE_PANE_LINES) {
        lines.push(Line::default());
    }
    lines
}

impl Component for FieldsDialog {
    type Hit = FieldsHit;
    type Open = Option<String>;

    fn open(&mut self, column: Option<String>, ctx: &mut Ctx<'_>) {
        self.open = true;
        self.top = 0;
        self.geometry = FieldsGeometry::default();
        self.stats = None;
        if let Some(state) = ctx.views.active_mut() {
            // Freeze the identity, not the current row projection. A cache miss
            // is pending work and must not prevent the dialog opening.
            state.field_picker_row = state.selected.clone();
            state.field_picker_selected = 0;
        }
        // §8.5: a list opens on the row the opening context names. The View
        // summary names a top-level column; a record that does not carry it
        // leaves the selection on the first row.
        if let Some(column) = column
            && let Some(row) = anchored_row(ctx.views, ctx.provider)
            && let Some(state) = ctx.views.active_mut()
            && let Some(index) = field_rows(&row, &state.expanded_paths)
                .iter()
                .position(|field| field.depth == 0 && top_level_key(&field.path) == column)
        {
            state.field_picker_selected = index;
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
                Outcome::Close
            }
            Event::Command(CommandId::PinField) => self.toggle_field(true, ctx),
            Event::Command(CommandId::ColorField) => self.toggle_field(false, ctx),
            Event::Command(CommandId::CorrelateField) => self.correlate(ctx),
            Event::Command(CommandId::FilterToFieldValue) => self.filter_to_value(false, ctx),
            Event::Command(CommandId::ExcludeFieldValue) => self.filter_to_value(true, ctx),
            Event::Command(CommandId::FoldByField) => self.fold_by_field(ctx),
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

    fn action_labels(&self, ctx: &Ctx<'_>) -> Vec<&'static str> {
        action_buttons(ctx.views, ctx.provider)
            .into_iter()
            .map(|(label, _)| label)
            .collect()
    }

    fn press_action(&mut self, index: usize, ctx: &mut Ctx<'_>) -> Outcome {
        let Some((_, control)) = action_buttons(ctx.views, ctx.provider).get(index).copied() else {
            return Outcome::Ignored;
        };
        match control {
            FieldPickerControl::Pin => self.toggle_field(true, ctx),
            FieldPickerControl::Color => self.toggle_field(false, ctx),
            FieldPickerControl::Severity => self.toggle_role(true, ctx),
            FieldPickerControl::Timestamp => self.toggle_role(false, ctx),
            FieldPickerControl::Filter => self.filter_to_value(false, ctx),
            FieldPickerControl::Exclude => self.filter_to_value(true, ctx),
            FieldPickerControl::Fold => self.fold_by_field(ctx),
            FieldPickerControl::Correlate => self.correlate(ctx),
            FieldPickerControl::Context => self.open_context(ctx),
            FieldPickerControl::List => Outcome::Ignored,
        }
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
        let width = content_width(area, DialogClass::L);
        let row = anchored_row(ctx.views, ctx.provider);
        let has_anchor = anchor_id(ctx.views).is_some();
        let title = match anchor_id(ctx.views) {
            Some(id) => format!("Fields · record {}", id.sequence),
            None => "Fields".to_owned(),
        };
        let expanded = ctx
            .views
            .active()
            .map(|state| state.expanded_paths.clone())
            .unwrap_or_default();
        let fields: Vec<FieldRow> = row
            .as_ref()
            .map_or_else(Vec::new, |row| field_rows(row, &expanded));
        let (state_word, sentence) = if row.is_none() && has_anchor {
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
        let help = if fields.is_empty() {
            ""
        } else {
            "Pinned fields become log columns; a nested value acts through its top-level field."
        };

        let control = ctx
            .views
            .active()
            .map_or(C::List, |state| state.field_picker_control);
        let selected = ctx
            .views
            .active()
            .map_or(0, |state| state.field_picker_selected)
            .min(fields.len().saturating_sub(1));
        let pinned = ctx
            .views
            .active()
            .map_or_else(Vec::new, |state| state.pinned_columns.clone());
        let color_field = ctx
            .views
            .active()
            .and_then(|state| state.color_field.clone());
        let selected_row = fields.get(selected).cloned();
        let actions = action_buttons(ctx.views, ctx.provider);
        let action_labels = actions.iter().map(|(label, _)| *label).collect::<Vec<_>>();

        // Whole-view figures, when a pass has answered for exactly this field.
        let whole_view = match (ctx.views.active_id(), selected_row.as_ref()) {
            (Some(view_id), Some(row)) => ctx
                .whole_view_stats
                .filter(|stats| stats.view_id == view_id && stats.path == row.path),
            _ => None,
        };
        // §8.12: statistics for the selected path, cached per revision.
        let stats = match (ctx.views.active_id(), selected_row.as_ref()) {
            (Some(view_id), Some(row)) if !fields.is_empty() => {
                let revision = ctx.provider.revision(view_id);
                Some(self.stats_for(view_id, revision, &row.path, ctx.provider))
            }
            _ => None,
        };

        let side_by_side = width >= SIDE_BY_SIDE_WIDTH;
        let list_rows = fields.len().clamp(1, 16) as u16;
        let value_rows = if fields.is_empty() {
            0
        } else {
            VALUE_PANE_LINES + 1
        };
        let body_rows = if side_by_side {
            (list_rows + 1).max(value_rows)
        } else {
            (list_rows.min(8) + 1) + u16::from(value_rows > 0) + value_rows
        };
        let content = DialogContent {
            header: 0,
            body: body_rows,
            message: if quiet {
                0
            } else {
                message_rows(&sentence, width)
            },
            help: help_rows(help, width),
            actions: packed_button_rows(width, &action_labels),
        };
        let regions = dialog_frame_regions(frame, area, DialogClass::L, &title, &content, theme);
        let surface = Surface {
            popup: regions.popup,
            interior: regions.interior,
            caret: None,
            scrollable: true,
            // Fields takes no text, so `q` dismisses it (§1).
            text_focus: false,
        };
        let body = regions.body;
        if body.width == 0 || body.height == 0 {
            return self.record(rows_hit, controls_hit, surface);
        }

        // Split the body into the list pane and the Value pane.
        let (list_area, value_area) = if value_rows == 0 {
            (body, Rect::new(body.x, body.y, 0, 0))
        } else if side_by_side {
            let list_width = body.width.saturating_sub(2) / 2;
            (
                Rect::new(body.x, body.y, list_width, body.height),
                Rect::new(
                    body.x + list_width + 2,
                    body.y,
                    body.width.saturating_sub(list_width + 2),
                    body.height.min(value_rows),
                ),
            )
        } else {
            let list_height = body.height.saturating_sub(value_rows + 1).max(2);
            (
                Rect::new(body.x, body.y, body.width, list_height),
                Rect::new(
                    body.x,
                    body.y + list_height + 1,
                    body.width,
                    body.height.saturating_sub(list_height + 1),
                ),
            )
        };

        let count = format!(
            "{} field{}",
            fields.len(),
            if fields.len() == 1 { "" } else { "s" }
        );
        let rects = pane(
            list_area,
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
            for (offset, (index, field)) in fields
                .iter()
                .enumerate()
                .skip(top)
                .take(visible)
                .enumerate()
            {
                let y = rects.viewport.y.saturating_add(offset as u16);
                let row_rect = Rect::new(rects.viewport.x, y, rects.viewport.width, 1);
                let focused = index == selected;
                let list_focus = focused && control == C::List;
                let style = if list_focus {
                    styles.selection
                } else if focused {
                    styles.label
                } else {
                    styles.description
                };
                // §8.4: the checkbox says whether the field's column is pinned;
                // the marker says which row the actions would act on; §8.11:
                // the disclosure says whether a container is open.
                let marker = if focused {
                    if ascii { "> " } else { "› " }
                } else {
                    "  "
                };
                let column = top_level_key(&field.path);
                let box_text = match &field.shape {
                    FieldShape::Container { expanded, .. } => {
                        format!(" {} ", disclosure(*expanded, ascii))
                    }
                    FieldShape::Scalar { .. } if field.depth == 0 => {
                        if pinned.iter().any(|value| value == column) {
                            "[x]"
                        } else {
                            "[ ]"
                        }
                        .to_owned()
                    }
                    FieldShape::Scalar { .. } => "   ".to_owned(),
                };
                let lead = format!("{marker}{box_text} ");
                let lead_width = (FIELD_CHECKBOX_WIDTH + 2).min(row_rect.width);
                frame.render_widget(
                    Paragraph::new(truncated(&lead, usize::from(lead_width))).style(style),
                    Rect::new(row_rect.x, y, lead_width, 1),
                );
                let name_x = row_rect.x.saturating_add(lead_width);
                let name_width = FIELD_NAME_WIDTH.min(row_rect.right().saturating_sub(name_x));
                let name = format!("{}{}", "  ".repeat(field.depth), field.label);
                frame.render_widget(
                    Paragraph::new(truncated(&name, usize::from(name_width))).style(style),
                    Rect::new(name_x, y, name_width, 1),
                );
                let value_x = name_x
                    .saturating_add(name_width)
                    .saturating_add(FIELD_GUTTER);
                if value_x < row_rect.right() {
                    let value_width = row_rect.right().saturating_sub(value_x);
                    let (shown, value_style) = match &field.shape {
                        FieldShape::Container { summary, .. } => (
                            summary.clone(),
                            if list_focus {
                                style
                            } else {
                                styles.description.add_modifier(Modifier::ITALIC)
                            },
                        ),
                        FieldShape::Scalar { text, kind } => (
                            if color_field.as_deref() == Some(column) && field.depth == 0 {
                                format!("{text} · colouring rows")
                            } else {
                                text.clone()
                            },
                            match kind {
                                Some(kind) if !list_focus => json_kind_style(kind, theme),
                                _ => style,
                            },
                        ),
                    };
                    frame.render_widget(
                        Paragraph::new(truncated(&shown, usize::from(value_width)))
                            .style(value_style),
                        Rect::new(value_x, y, value_width, 1),
                    );
                }
                rows_hit.push((row_rect, index));
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

        // §8.12: the Value pane, a fixed-height region.
        if value_area.width > 0 && value_area.height > 0 {
            let heading = selected_row
                .as_ref()
                .map(|row| format!("Value · {}", row.path))
                .unwrap_or_else(|| "Value".to_owned());
            // The note says what the figures rest on, and says so before they
            // change: a reader who sees a distinct count go from "4,096+" to an
            // exact number should be able to see why from the same line.
            let sample_note = match (whole_view.as_ref(), ctx.field_stats_pending) {
                (Some(whole), _) => format!(
                    "all {} records",
                    thousands(usize::try_from(whole.records).unwrap_or(usize::MAX))
                ),
                (None, true) => format!(
                    "first {} records · counting the rest",
                    thousands(MAX_STATS_ROWS)
                ),
                (None, false) => format!("first {} records", thousands(MAX_STATS_ROWS)),
            };
            let value_rects = pane(
                value_area,
                u16::try_from(UnicodeWidthStr::width(sample_note.as_str())).unwrap_or(0),
                usize::from(VALUE_PANE_LINES),
            );
            if value_rects.heading.height > 0 {
                frame.render_widget(
                    Paragraph::new(truncated(
                        &heading,
                        usize::from(
                            value_rects
                                .heading
                                .width
                                .saturating_sub(value_rects.count.width + 1),
                        ),
                    ))
                    .style(styles.label.add_modifier(Modifier::BOLD)),
                    value_rects.heading,
                );
                if value_rects.count.width > 0 {
                    frame.render_widget(
                        Paragraph::new(sample_note).style(styles.description),
                        value_rects.count,
                    );
                }
            }
            let lines = value_pane_lines(
                stats.as_ref(),
                whole_view.as_ref().copied(),
                selected_row.as_ref(),
                theme,
            );
            frame.render_widget(Paragraph::new(lines), value_rects.viewport);
        }

        if !quiet {
            render_message(frame, regions.message, state_word, &sentence, theme, ascii);
        }
        render_help_text(frame, regions.help, help, theme);
        let focused = actions
            .iter()
            .position(|(_, candidate)| *candidate == control);
        // §8.9: Pin is the default; it is first in the row.
        for (index, rect) in render_actions(
            frame,
            regions.actions,
            ActionRow {
                labels: &action_labels,
                default: Some(0),
                destructive: &[],
                focused,
            },
            theme,
        ) {
            controls_hit.push((rect, actions[index].1));
        }
        self.record(rows_hit, controls_hit, surface)
    }
}
