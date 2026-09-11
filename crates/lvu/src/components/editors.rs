//! The query editors — the two-tab Filter dialog (`/`: Search and Advanced)
//! and multiline Grouping (`m`) — converted to the component contract per
//! `docs/component-model.md` §6.3 step 7.
//!
//! One type, two permanent slots. Search, Advanced and Grouping are the same
//! dialog with a different `QueryPurpose`: the same keymap, the same caret
//! rules, the same submit path and the same message row, differing only in
//! title, help, placeholder and what the body draws beside the field.
//! Splitting them into three types would have duplicated all of that to
//! express three constants.
//!
//! Search and Advanced share one slot and one layer (§12.1): the Filter dialog
//! has a `Search │ Advanced` segmented control in its header, and the slot's
//! `purpose` is whichever tab is active. Both constraints stay applied at once,
//! combined with AND, exactly as before; the title names what is applied so the
//! user can see both from either tab.
//!
//! This is the first conversion whose drafts are *view*-owned rather than
//! dialog-owned (§2.5). `ViewState.{search,advanced,grouping}` keeps the draft,
//! the last accepted value, the error and the pending fence; the caret lives in
//! `ctx.cursors` under the same `TextTarget` the shell used. Nothing here
//! caches a copy: the layer renders from `ctx.views.active()` every frame, so a
//! query completion landing in `ViewState` reaches the screen with no event.
//!
//! The two product invariants this layer carries (AGENTS.md):
//!
//! * **An invalid draft leaves the last applied view usable.** The draft and
//!   the accepted value are separate fields; a refusal writes `editor.error`
//!   and touches neither `applied` nor the rows on screen.
//! * **A canonical view forks instead of filtering in place.** `Views::enqueue`
//!   stages the derived view itself and reports success, so the layer stays
//!   open on the view its edit created and never has to know a fork happened
//!   (§2.3). Live debounced search never forks; it goes through the shell's
//!   `enqueue_live_query`, which is why the layer only *arms* the debounce and
//!   never fires it.

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Paragraph},
};
use unicode_width::UnicodeWidthStr;

use crate::app::{
    EditorCompletionKind, EditorCompletionState, EditorState, MAX_EDITOR_BYTES, QueryPurpose,
    Views, sample_editor_completion,
};
use crate::command_palette::CommandId;
use crate::component::{
    CommandEntry, CommandSpec, Component, Ctx, Event, Outcome, RenderCtx, Surface, ViewEvent,
    is_typed_char,
};
use crate::dialog_controls::DialogStyles;
use crate::dialog_controls::render_role_button;
use crate::dialog_controls::{ButtonRole, mnemonic, role_style};
use crate::dialog_layout::{ContextFootprint, DialogSpec, PresentationKind, ScrollViewport};
use crate::text_edit::{EditCommand, EditPolicy, TextTarget, edit};
use crate::theme::Theme;
use crate::ui::{
    InputSurface, MESSAGE_SENTENCE_COLUMN, MessageState, clear_themed,
    draw_editor_completion_anchored, place_input_cursor_at, render_help_text, render_message,
    render_placeholder, render_responsive_frame, render_scrollbar, render_segmented_control,
    truncated, wrap_sentence,
};
use ratatui::widgets::Widget;

/// §12.3. The preview pane is fixed content, so it is measured, not guessed.
const GROUPING_PREVIEW: [&str; 2] = ["RuntimeException: boom", "  at worker.rs:42"];

/// The two tabs of the Filter dialog, in segment order (§12.1). Each is a
/// `QueryPurpose`, because a tab *is* the editor for that purpose.
const FILTER_TABS: [QueryPurpose; 2] = [QueryPurpose::Search, QueryPurpose::Advanced];

/// Segment labels with their §8.10 mnemonics: Alt-S and Alt-A switch tabs
/// from anywhere in the dialog, including from inside the field, and the bare
/// letters do once the field no longer has the keys. The shell resolves them
/// through `action_labels`, the same way it presses a button.
const FILTER_TAB_LABELS: [&str; 2] = ["&Search", "&Advanced"];
/// The Grouping dialog's normal control: Run and Filter are the configured
/// enrichment rules (consecutive equal keys; non-null enrichment opens an
/// event), Off clears the rule, and Legacy is the explicit compat home for
/// restored Auto tokens and custom continuation regexes — never a default.
const GROUPING_MODE_LABELS: [&str; 4] = ["Run", "Filter", "Legacy", "Off"];

/// Everything the Filter dialog lets a mnemonic press, in `action_labels`
/// order: the two buttons, then the two segments (as View lists its mode
/// controls beside its buttons).
const FILTER_MNEMONIC_TARGETS: [&str; 4] = ["Apply", "&Clear", "&Search", "&Advanced"];

/// The Filter action row. `Apply` is the default (§8.9) and Enter presses it;
/// `Clear` empties the active tab's draft and applies that, which is how a
/// constraint is removed without removing the other tab's.
const FILTER_ACTIONS: [&str; 2] = ["Apply", "&Clear"];

/// Grouping draws one button and it carries no mnemonic: Enter is its key.
const GROUPING_ACTIONS: [&str; 1] = ["Apply"];

/// §8.9 roles for the Grouping action row, parallel to [`GROUPING_ACTIONS`].
const GROUPING_ROLES: [ButtonRole; 1] = [ButtonRole::Default];

/// §8.9 roles for the Filter action row, parallel to [`FILTER_ACTIONS`]:
/// `Apply` is the filled default, `Clear` is ordinary (never destructive).
const FILTER_ROLES: [ButtonRole; 2] = [ButtonRole::Default, ButtonRole::Normal];

/// Shared anchored action-overflow (`More ▾`) menu for the contextual-editor
/// cohort: Filter, Time and Colour rules.
///
/// The generic half of the overflow contract, living here because all three
/// consumers are this cohort's (a future home is `dialog_controls` beside
/// [`crate::dialog_controls::MORE_LABEL`], owned by the core slice). Each
/// dialog owns its open/selection state and routes activation through its own
/// `press_action` with ORIGINAL action indices; this function owns the one
/// shared placement and paint path. `labels`/`roles` are parallel to the
/// dialog's full action row, `overflow` names the hidden subset in display
/// order, and `selected` is a position within `overflow`.
///
/// Placement comes from shared `anchored_geometry` against the painted `More`
/// button rect — below preferred with a one-row gap, above when below cannot
/// fit, edge-clamped, max eight rows — bounded by the full render area, so the
/// menu may overhang its parent dialog. The same geometry drives paint, the
/// returned row hitboxes (carrying ORIGINAL indices for `press_action`) and
/// the scrollbar. Returns the popup rect (empty when refused), the row
/// hitboxes, the scroll offset to retain, and whether the list overflows.
///
/// Class-A contract, enforced like the completion popup: without a one-row
/// gap to the anchor, or below the 3x3 paint minimum, nothing paints.
/// Callers treat an empty popup as "not drawn" for containment, selection
/// and mouse. Hand-rolled row painting here is presentation-only folding: it
/// maps stable labels to painted cells, never query membership (AGENTS.md).
/// The ten parameters are each load-bearing (frame, bound, anchor, content,
/// selection, scroll, theme), matching the painter precedent across this
/// codebase.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_action_menu(
    frame: &mut Frame<'_>,
    area: Rect,
    anchor: Rect,
    labels: &[&str],
    roles: &[ButtonRole],
    overflow: &[usize],
    selected: usize,
    desired_first: usize,
    theme: Theme,
    ascii: bool,
) -> (Rect, Vec<(Rect, usize)>, usize, bool) {
    if overflow.is_empty() || anchor.is_empty() {
        return (Rect::default(), Vec::new(), 0, false);
    }
    let longest = overflow
        .iter()
        .filter_map(|&index| labels.get(index))
        .map(|label| UnicodeWidthStr::width(mnemonic(label).text.as_str()))
        .max()
        .unwrap_or(0);
    let preferred = u16::try_from(longest.saturating_add(4)).unwrap_or(u16::MAX);
    let spec = crate::dialog_layout::AnchoredSpec::new(overflow.len(), None, preferred, 0);
    let selected = selected.min(overflow.len() - 1);
    let geometry =
        crate::dialog_layout::anchored_geometry(area, anchor, &spec, selected, desired_first);
    let popup = geometry.popup;
    let gap_below = popup.y == anchor.bottom().saturating_add(1);
    let gap_above = popup.bottom().saturating_add(1) == anchor.y;
    if popup.width < 3 || popup.height < 3 || !(gap_below || gap_above) {
        return (Rect::default(), Vec::new(), 0, false);
    }
    clear_themed(frame, popup, theme);
    let styles = DialogStyles::new(theme);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.accent)),
        popup,
    );
    let viewport = geometry.viewport;
    if viewport.width == 0 || viewport.height == 0 {
        return (Rect::default(), Vec::new(), 0, false);
    }
    let row_width = viewport
        .width
        .saturating_sub(u16::from(geometry.scrollbar.is_some()));
    let mut rows: Vec<(Rect, usize)> = Vec::new();
    let mut lines: Vec<Line<'_>> = Vec::new();
    for (offset, pos) in (geometry.first_item..overflow.len())
        .take(usize::from(viewport.height))
        .enumerate()
    {
        let original = overflow[pos];
        let text = labels
            .get(original)
            .map(|label| mnemonic(label).text)
            .unwrap_or_default();
        let role = roles.get(original).copied().unwrap_or(ButtonRole::Normal);
        let highlighted = pos == selected;
        let style = if highlighted {
            styles.selection
        } else {
            role_style(theme, role, false)
        };
        lines.push(Line::styled(
            format!(
                "{} {}",
                if highlighted { ">" } else { " " },
                truncated(&text, usize::from(row_width.saturating_sub(2)))
            ),
            style,
        ));
        rows.push((
            Rect::new(
                viewport.x,
                viewport.y.saturating_add(offset as u16),
                row_width,
                1,
            ),
            original,
        ));
    }
    frame.render_widget(
        Paragraph::new(lines),
        Rect::new(viewport.x, viewport.y, row_width, viewport.height),
    );
    if let Some(bar) = geometry.scrollbar {
        let limit = overflow.len().saturating_sub(usize::from(viewport.height));
        render_scrollbar(frame, bar, geometry.first_item, limit, theme, ascii);
    }
    (
        popup,
        rows,
        geometry.first_item,
        geometry.scrollbar.is_some(),
    )
}

/// Stable responsive budgets for the contextual editors.
///
/// Both Filter tabs and Grouping are `Contextual::Prompt`: one live field with
/// a top-biased frame centered over the frozen log pane when retained. Outer
/// size comes from the presentation policy plus these stable maxima alone —
/// never from the current diagnostic length, pending state or preview rows —
/// so valid/invalid/pending/diagnostic frames share one `frame` and sticky
/// tail origins. `body_content_rows` sizes only the shared scroll extent.
///
/// Hand-rolled budget choice here is presentation-only folding: it reserves
/// stable chrome rows, never query membership (AGENTS.md).
fn filter_spec(area: Rect) -> DialogSpec {
    let (message, help, actions) = responsive_chrome(
        area,
        PresentationKind::Contextual(ContextFootprint::Prompt),
        1,
        2,
        2,
        0,
        &FILTER_ACTIONS,
    );
    DialogSpec::new(
        PresentationKind::Contextual(ContextFootprint::Prompt),
        1,
        1,
        message,
        help,
        actions,
    )
}

fn grouping_spec(area: Rect) -> DialogSpec {
    let (message, help, actions) = responsive_chrome(
        area,
        PresentationKind::Contextual(ContextFootprint::Prompt),
        1,
        2,
        2,
        1,
        &GROUPING_ACTIONS,
    );
    DialogSpec::new(
        PresentationKind::Contextual(ContextFootprint::Prompt),
        1,
        1,
        message,
        help,
        actions,
    )
}

/// Area-aware stable chrome budgets for the contextual-editor cohort: Filter,
/// Time, Colour rules (and Grouping, where the rule is a no-op).
///
/// `actions` comes from shared `stable_action_rows` against the policy
/// content width, capped to two: one row wherever the verbs share it (roomy
/// sizes keep no dead second row), two where they wrap. `message`/`help` keep
/// their normal maxima — except that a two-row action band at a short height
/// pre-sheds help, then message down to `floor_message`, instead of letting
/// degradation shed the action rows first and strand full-width controls
/// (at the 20x6 floor: Filter header1 + body1 + actions2 message/help
/// dropped; Time/Colour body1 + message1 + actions2, no help).
/// Degradation then sheds nothing further, so the kept anatomy always equals
/// the spec: budgets are stable per viewport across every async state, while
/// roomy viewports keep full message/help with one action row bit-for-bit.
///
/// The content width/height are measured through the same shared
/// `frame_block(is_compact(area))` the renderer lays out from — never
/// hard-coded border/padding subtraction — so the estimate cannot drift from
/// the laid-out content when the frame tokens change.
///
/// Hand-rolled budget choice here is presentation-only folding, never query
/// membership (AGENTS.md).
pub(crate) fn responsive_chrome(
    area: Rect,
    kind: PresentationKind,
    header_rows: u16,
    normal_message: u16,
    normal_help: u16,
    floor_message: u16,
    labels: &[&str],
) -> (u16, u16, u16) {
    use crate::dialog_controls::stable_action_rows;
    use crate::dialog_layout::{frame_block, is_compact, policy_size};
    let (policy_width, policy_height) = policy_size(area, kind);
    // Exact policy content box: the same Block resolves against, so the
    // budget sees precisely the cells the dialog will split.
    let content_probe = frame_block(is_compact(area)).inner(Rect::new(
        0,
        0,
        policy_width.max(1),
        policy_height.max(1),
    ));
    let content_width = content_probe.width.max(1);
    let content_height = content_probe.height;
    let actions = stable_action_rows(content_width, labels).clamp(1, 2);
    let (normal_message, normal_help) = (normal_message.min(2), normal_help.min(2));
    if actions == 2 && content_height < header_rows + 1 + normal_message + normal_help + actions {
        return (floor_message.min(normal_message), 0, actions);
    }
    (normal_message, normal_help, actions)
}

/// Everything an editor draws that can be clicked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EditorHit {
    /// A row of the completion popup.
    Completion(usize),
    /// A row of the open action-overflow menu, by ORIGINAL action index
    /// (into [`FILTER_ACTIONS` / `GROUPING_ACTIONS` for `press_action`).
    More(Option<usize>),
    /// A segment of the Filter dialog's `Search │ Advanced` control.
    Tab(usize),
    /// The drawn `[ Apply ]` button.
    Apply,
    /// The drawn `[ Clear ]` button, which only Filter has.
    Clear,
    /// The overflowing-diagnostic pane.
    Diagnostics,
    Body,
}

/// Which control has the keys (§8.8). `Field` is the text input; `Tabs` is
/// the segmented control, where Left/Right switch tabs; `Pane` hands the
/// arrows to the overflowing-diagnostic pane.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum EditorFocus {
    #[default]
    Field,
    Tabs,
    Pane,
}

/// One stop of the Tab cycle. The completion popup counts as two stops on
/// Advanced (fields, then sampled literals), so that Tab keeps its documented
/// meaning there: `Fields and sampled literals complete with Tab`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FocusStop {
    Field,
    Completion(EditorCompletionKind),
    Tabs,
    Pane,
}

/// Recorded by `render`, consumed by `hit()` and by the scroll logic (§5.1).
#[derive(Clone, Debug, Default)]
struct EditorGeometry {
    body: Rect,
    /// Last painted field rect (body row 0 through the shared viewport).
    field: Rect,
    completion: Vec<(Rect, usize)>,
    /// The anchored completion popup rect actually painted this frame, or
    /// empty when no popup was drawn. Hit-testing and mouse routing consult
    /// this — never the completion state alone — so an undrawn popup claims
    /// no clicks.
    completion_popup: Rect,
    /// The painted `More ▾` button rect, if the shared action geometry planned
    /// one this frame. Toggles the overflow menu; empty when every action fit.
    more_button: Option<Rect>,
    /// Overflow action indices hidden behind `More ▾` this frame, in display
    /// order. Event paths (open/nav/activate) read this copy; paint reads the
    /// live action geometry.
    more_overflow: Vec<usize>,
    /// Painted overflow-menu row hitboxes with ORIGINAL action indices for
    /// `press_action`. Empty unless the menu painted.
    more_rows: Vec<(Rect, usize)>,
    tabs: Vec<Rect>,
    /// Button rects in `FILTER_ACTIONS` order (Grouping draws only `Apply`).
    actions: Vec<Rect>,
    /// The resolved sticky action band (kept rows after pressure).
    action_band: Rect,
    diagnostics: Option<Rect>,
}

#[derive(Debug)]
pub struct EditorDialog {
    /// Which editor this slot currently is. Fixed for Grouping; for the Filter
    /// slot it is the active tab and changes with it.
    purpose: QueryPurpose,
    /// Whether this slot is the two-tab Filter dialog (§12.1) rather than the
    /// single-purpose Grouping editor.
    tabbed: bool,
    open: bool,
    /// Body scroll for the overflowing-diagnostic pane, and which control has
    /// the keys. The scroll was `App::dialog_scroll*`, shell state three
    /// dialogs shared by accident of focus.
    scroll: usize,
    scroll_limit: usize,
    focus: EditorFocus,
    /// The completion popup, which only Advanced offers (§12.1). Its state is
    /// fenced against the draft and caret it was built for, so accepting a
    /// stale entry is impossible.
    completion: Option<EditorCompletionState>,
    next_completion_generation: u64,
    /// The action-overflow (`More ▾`) menu: open flag, selection as a position
    /// within the overflow list, and retained scroll offset. Activation routes
    /// through `press_action` with original action indices, so menu items run
    /// exactly what their buttons would.
    more_open: bool,
    more_selected: usize,
    more_first: usize,
    geometry: EditorGeometry,
    surface: Surface,
}

impl EditorDialog {
    /// A single-purpose editor slot: Grouping.
    pub fn new(purpose: QueryPurpose) -> Self {
        Self {
            purpose,
            tabbed: false,
            open: false,
            scroll: 0,
            scroll_limit: 0,
            focus: EditorFocus::Field,
            completion: None,
            next_completion_generation: 1,
            more_open: false,
            more_selected: 0,
            more_first: 0,
            geometry: EditorGeometry::default(),
            surface: Surface::default(),
        }
    }

    /// The Filter slot: Search and Advanced as two tabs of one dialog. It
    /// opens on Search unless asked for the other tab.
    pub fn filter() -> Self {
        Self {
            tabbed: true,
            ..Self::new(QueryPurpose::Search)
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// The editor this slot is submitting under: the active tab on Filter.
    pub fn purpose(&self) -> QueryPurpose {
        self.purpose
    }

    pub fn scroll(&self) -> usize {
        self.scroll
    }

    pub fn scroll_limit(&self) -> usize {
        self.scroll_limit
    }

    /// Whether the overflowing-diagnostic pane has the arrows.
    pub fn scroll_focused(&self) -> bool {
        self.focus == EditorFocus::Pane
    }

    /// Whether the `Search │ Advanced` control has the keys.
    pub fn tabs_focused(&self) -> bool {
        self.focus == EditorFocus::Tabs
    }

    pub fn completion(&self) -> Option<&EditorCompletionState> {
        self.completion.as_ref()
    }

    pub fn completion_rects(&self) -> &[(Rect, usize)] {
        &self.geometry.completion
    }

    /// The anchored completion popup rect painted on the last render, or empty
    /// when no popup was drawn (e.g. the field had no painted rect). Tests use
    /// this to assert the actual-field anchor and the frame-bounded placement.
    pub fn completion_popup(&self) -> Rect {
        self.geometry.completion_popup
    }

    /// The painted `More ▾` button rect, if overflow planned one this frame.
    /// Tests use this to assert no invented overflow at roomy sizes.
    pub fn more_button(&self) -> Option<Rect> {
        self.geometry.more_button
    }

    /// Painted overflow-menu rows with original action indices. Empty unless
    /// the menu painted (unreachable with current budgets, retained as the
    /// refusal-side record alongside `more_button`).
    pub fn more_rows(&self) -> &[(Rect, usize)] {
        &self.geometry.more_rows
    }

    /// The resolved sticky action band this frame (kept rows after pressure).
    /// Tests assert its height equals the requested budget: degradation must
    /// never silently shrink it.
    pub fn action_band(&self) -> Rect {
        self.geometry.action_band
    }

    /// Whether the overflow menu is open (state, independent of paint).
    pub fn more_open(&self) -> bool {
        self.more_open
    }

    /// The field rect painted on the last render (body row 0 projected through
    /// the shared viewport), or empty when the field had no rows. Tests use
    /// this as the anchor side of the one-row-gap assertion.
    pub fn field_rect(&self) -> Rect {
        self.geometry.field
    }

    pub fn action_rects(&self) -> &[Rect] {
        &self.geometry.actions
    }

    /// The segment rects of the tab control, in `FILTER_TABS` order.
    pub fn tab_rects(&self) -> &[Rect] {
        &self.geometry.tabs
    }

    /// The overflowing-diagnostic pane, when the message did not fit. `None`
    /// means the layer claims no scroll affordance (§9).
    pub fn diagnostics_rect(&self) -> Option<Rect> {
        self.geometry.diagnostics
    }

    /// The caret target for this view's draft. View-owned, so it is the bank's
    /// (§2.2) and survives closing and reopening the dialog — and switching
    /// tabs, since each tab keys its own.
    fn target(&self, view_id: &str) -> TextTarget {
        TextTarget {
            identity: view_id.to_owned(),
            field: match self.purpose {
                QueryPurpose::Search => "search",
                QueryPurpose::Grouping => "grouping",
                _ => "advanced",
            },
        }
    }

    /// Whether the field, rather than the tabs or the diagnostics pane, has
    /// the keys. This is what `App::active_text_target` returned `None` for
    /// when `dialog_scroll_focused` was set.
    fn text_editing(&self) -> bool {
        self.focus == EditorFocus::Field
    }

    fn draft(&self, ctx: &Ctx<'_>) -> Option<(String, String)> {
        let view_id = ctx.views.active_id()?.to_owned();
        let draft = ctx.views.editor(&view_id, self.purpose)?.draft.clone();
        Some((view_id, draft))
    }

    fn policy(&self) -> EditPolicy {
        EditPolicy {
            max_bytes: MAX_EDITOR_BYTES,
            multiline: self.purpose != QueryPurpose::Search,
        }
    }

    /// One editing verb against the view-owned draft and the bank-owned caret.
    /// The two are read and written in sequence, never held at once (§2.5).
    fn text(&mut self, command: EditCommand<'_>, ctx: &mut Ctx<'_>) -> Outcome {
        if !self.text_editing() {
            return Outcome::Ignored;
        }
        let Some((view_id, mut value)) = self.draft(ctx) else {
            return Outcome::Ignored;
        };
        let target = self.target(&view_id);
        if self.purpose == QueryPurpose::Grouping && value == crate::grouping::AUTO_GROUPING_TOKEN {
            match command {
                EditCommand::Insert(_) => {
                    value.clear();
                    ctx.cursors.reset(target.clone(), "");
                }
                EditCommand::Backspace | EditCommand::KillToEndOfLine => {
                    return self.set_grouping_mode(3, true, ctx);
                }
                _ => {
                    ctx.cursors.reset(target, "");
                    return Outcome::Consumed;
                }
            }
        }
        // Erasing in a blank configured rule clears it: a rule that names no
        // column cannot apply, and Off is where empty drafts belong.
        if self.purpose == QueryPurpose::Grouping
            && (value == crate::grouping::run_rule("") || value == crate::grouping::filter_rule(""))
            && matches!(
                command,
                EditCommand::Backspace | EditCommand::KillToEndOfLine
            )
        {
            return self.set_grouping_mode(3, true, ctx);
        }
        let mut cursor = ctx.cursors.get_or_end(target.clone(), &value);
        let outcome = edit(&mut value, &mut cursor, command, self.policy());
        ctx.cursors.store(target, cursor);
        if outcome.changed {
            if let Some(editor) = ctx.views.editor_mut(&view_id, self.purpose) {
                editor.draft = value;
                // Only legacy custom text is remembered for the Legacy tab;
                // configured tokens are rebuilt by the tab and the column
                // cycler, so remembering them here would resurface a stale
                // column under the wrong tab.
                if self.purpose == QueryPurpose::Grouping
                    && matches!(
                        crate::grouping::parse_grouping(&editor.draft),
                        Ok(crate::grouping::GroupingSpec::Custom(_))
                    )
                {
                    editor.custom_grouping_draft = Some(editor.draft.clone());
                }
            }
            ctx.views.touch(&view_id);
            // A changed draft invalidates the popup it was offered against.
            self.completion = None;
            if self.purpose == QueryPurpose::Search {
                ctx.views.schedule_search(&view_id);
            }
        }
        if outcome.changed || outcome.moved {
            Outcome::Consumed
        } else {
            Outcome::Ignored
        }
    }

    /// Apply the draft. The refusal paths are the invariant: a full queue keeps
    /// the draft and says so, and a fixed definition becomes a derived view
    /// rather than a filter applied to All events.
    fn submit(&mut self, ctx: &mut Ctx<'_>) -> Outcome {
        let Some(view_id) = ctx.views.active_id().map(str::to_owned) else {
            return Outcome::Consumed;
        };
        if self.purpose == QueryPurpose::Search {
            // An explicit apply supersedes the armed debounce; leaving it would
            // fire a second, identical query a moment later.
            ctx.views.cancel_scheduled_search(&view_id);
        }
        ctx.views.touch(&view_id);
        // A full queue is worded in the editor's own message row by the seam,
        // which kept the draft; a fixed definition becomes a derived view
        // inside the seam. Either way the layer stays open, because applying a
        // filter on All events leaves the user in the editor, on the view it
        // created.
        let _ = ctx.views.enqueue(&view_id, self.purpose, None);
        Outcome::Consumed
    }

    /// `[ Clear ]`: empty the active tab's draft and, when that tab has a
    /// constraint applied, apply the empty draft so the constraint goes. A
    /// tab with nothing applied only loses its draft — submitting an empty
    /// value there would do nothing on a derived view and would fork a
    /// pointless derived view off All events.
    fn clear(&mut self, ctx: &mut Ctx<'_>) -> Outcome {
        let Some(view_id) = ctx.views.active_id().map(str::to_owned) else {
            return Outcome::Consumed;
        };
        let Some(editor) = ctx.views.editor_mut(&view_id, self.purpose) else {
            return Outcome::Consumed;
        };
        let applied = !editor.applied.is_empty();
        let changed = !editor.draft.is_empty() || editor.error.is_some();
        editor.draft.clear();
        editor.error = None;
        ctx.cursors.reset(self.target(&view_id), "");
        self.completion = None;
        self.focus = EditorFocus::Field;
        if applied {
            return self.submit(ctx);
        }
        if changed {
            ctx.views.touch(&view_id);
        }
        if self.purpose == QueryPurpose::Search {
            ctx.views.cancel_scheduled_search(&view_id);
        }
        Outcome::Consumed
    }

    /// Switch the Filter dialog to `purpose`'s tab. `to_field` puts the keys
    /// back in the field (a mnemonic or a click); Left/Right on the focused
    /// control keep the control focused so the user can keep moving.
    fn switch_tab(&mut self, purpose: QueryPurpose, to_field: bool) -> Outcome {
        if !self.tabbed || !FILTER_TABS.contains(&purpose) {
            return Outcome::Ignored;
        }
        if to_field {
            self.focus = EditorFocus::Field;
        }
        if purpose == self.purpose {
            return Outcome::Consumed;
        }
        self.purpose = purpose;
        // The popup, the pane scroll and its focus all belong to the tab that
        // is leaving; the one arriving is laid out fresh on the next frame.
        self.completion = None;
        self.scroll = 0;
        self.scroll_limit = 0;
        if self.focus == EditorFocus::Pane {
            self.focus = EditorFocus::Field;
        }
        Outcome::Consumed
    }

    fn move_tab(&mut self, delta: i32, ctx: &mut Ctx<'_>) -> Outcome {
        if self.purpose == QueryPurpose::Grouping {
            let current = self
                .draft(ctx)
                .map_or(3, |(_, draft)| grouping_mode(&draft));
            let next =
                (current as i32 + delta).rem_euclid(GROUPING_MODE_LABELS.len() as i32) as usize;
            return self.set_grouping_mode(next, false, ctx);
        }
        let Some(index) = FILTER_TABS.iter().position(|tab| *tab == self.purpose) else {
            return Outcome::Consumed;
        };
        let next = (index as i32 + delta).rem_euclid(FILTER_TABS.len() as i32) as usize;
        self.switch_tab(FILTER_TABS[next], false)
    }

    fn set_grouping_mode(&mut self, mode: usize, to_field: bool, ctx: &mut Ctx<'_>) -> Outcome {
        if self.purpose != QueryPurpose::Grouping {
            return Outcome::Ignored;
        }
        let Some(view_id) = ctx.views.active_id().map(str::to_owned) else {
            return Outcome::Consumed;
        };
        let current = ctx
            .views
            .editor(&view_id, QueryPurpose::Grouping)
            .map_or(3, |editor| grouping_mode(&editor.draft));
        if current == mode {
            if to_field {
                self.focus = EditorFocus::Field;
            }
            return Outcome::Consumed;
        }
        let remembered_custom = ctx
            .views
            .editor(&view_id, QueryPurpose::Grouping)
            .and_then(|editor| editor.custom_grouping_draft.clone());
        // Run and Filter start blank: Up/Down names the enrichment column,
        // and the caret opens inside the column slot so typing does too.
        let value = match mode {
            0 => crate::grouping::run_rule(""),
            1 => crate::grouping::filter_rule(""),
            2 => {
                remembered_custom.unwrap_or_else(|| crate::grouping::AUTO_GROUPING_TOKEN.to_owned())
            }
            _ => String::new(),
        };
        if let Some(editor) = ctx.views.editor_mut(&view_id, QueryPurpose::Grouping) {
            if current == 2
                && matches!(
                    crate::grouping::parse_grouping(&editor.draft),
                    Ok(crate::grouping::GroupingSpec::Custom(_))
                )
            {
                editor.custom_grouping_draft = Some(editor.draft.clone());
            }
            editor.draft.clone_from(&value);
            editor.error = None;
        }
        self.reset_grouping_caret(ctx, &view_id, &value, mode == 0 || mode == 1);
        self.completion = None;
        if to_field {
            self.focus = EditorFocus::Field;
        }
        ctx.views.touch(&view_id);
        Outcome::Consumed
    }

    /// The caret opens inside a configured rule's column slot (before the
    /// closing paren) so typing names the column; anywhere else it opens at
    /// the end as before.
    fn reset_grouping_caret(&self, ctx: &mut Ctx<'_>, view_id: &str, value: &str, in_slot: bool) {
        let target = self.target(view_id);
        ctx.cursors.reset(target.clone(), value);
        if in_slot && value.ends_with(')') {
            ctx.cursors.store(
                target,
                crate::text_edit::TextCursor {
                    char_index: value.chars().count().saturating_sub(1),
                },
            );
        }
    }

    /// Up/Down names the enrichment column of a Run/Filter draft by cycling
    /// the columns the view actually carries. Manual edits still work: the
    /// draft is the plain rule token. Anything the worker rejects (a column
    /// no accepted enrichment produces) fails on apply with its cause, and
    /// the last applied grouping stays.
    fn cycle_grouping_column(&mut self, delta: i32, ctx: &mut Ctx<'_>) -> Outcome {
        let Some(view_id) = ctx.views.active_id().map(str::to_owned) else {
            return Outcome::Consumed;
        };
        let draft = ctx
            .views
            .editor(&view_id, QueryPurpose::Grouping)
            .map(|editor| editor.draft.clone())
            .unwrap_or_default();
        let kind = if draft.starts_with(crate::grouping::RUN_NAMESPACE) {
            0
        } else if draft.starts_with(crate::grouping::FILTER_NAMESPACE) {
            1
        } else {
            return Outcome::Consumed;
        };
        // Only offer names that form a valid rule; anything else the
        // worker would reject on apply, so offering it picks a failure.
        let columns: Vec<String> = super::folding::fold_columns(ctx.views, ctx.provider)
            .into_iter()
            .filter(|name| crate::grouping::is_grouping_column_name(name))
            .collect();
        if columns.is_empty() {
            return Outcome::Consumed;
        }
        let current = if kind == 0 {
            draft
                .strip_prefix(crate::grouping::RUN_NAMESPACE)
                .and_then(|suffix| suffix.strip_suffix(')'))
                .unwrap_or_default()
        } else {
            draft
                .strip_prefix(crate::grouping::FILTER_NAMESPACE)
                .and_then(|suffix| suffix.strip_suffix(')'))
                .unwrap_or_default()
        };
        let next = columns
            .iter()
            .position(|name| name == current)
            .map_or(0, |index| {
                (index as i32 + delta).rem_euclid(columns.len() as i32) as usize
            });
        let value = if kind == 0 {
            crate::grouping::run_rule(&columns[next])
        } else {
            crate::grouping::filter_rule(&columns[next])
        };
        if let Some(editor) = ctx.views.editor_mut(&view_id, QueryPurpose::Grouping) {
            editor.draft.clone_from(&value);
            editor.error = None;
        }
        self.reset_grouping_caret(ctx, &view_id, &value, kind == 0 || kind == 1);
        self.completion = None;
        ctx.views.touch(&view_id);
        Outcome::Consumed
    }

    /// The completion popup's kind, when it is still the one offered for the
    /// current draft and caret; anything typed since makes it stale.
    fn current_completion_kind(
        &self,
        view_id: &str,
        draft: &str,
        target: &TextTarget,
        cursor: usize,
    ) -> Option<EditorCompletionKind> {
        self.completion
            .as_ref()
            .filter(|state| {
                state.view_id == view_id
                    && state.purpose == self.purpose
                    && state.draft == draft
                    && state.target == *target
                    && state.cursor == cursor
            })
            .map(|state| state.kind)
    }

    /// The Tab order (§8.8). Field first; on Advanced the two completion
    /// offers; then the tab control on Filter; then the diagnostics pane when
    /// there is one to scroll (Grouping keeps its unconditional pane stop, as
    /// before).
    fn focus_stops(&self) -> Vec<FocusStop> {
        let mut stops = vec![FocusStop::Field];
        if self.purpose == QueryPurpose::Advanced {
            stops.push(FocusStop::Completion(EditorCompletionKind::Field));
            stops.push(FocusStop::Completion(EditorCompletionKind::SampledValue));
        }
        if self.tabbed || self.purpose == QueryPurpose::Grouping {
            stops.push(FocusStop::Tabs);
        }
        if !self.tabbed || self.scroll_limit > 0 {
            stops.push(FocusStop::Pane);
        }
        stops
    }

    /// Tab and Shift-Tab. On Search and Grouping there is nothing to complete,
    /// so Tab reaches the tab control or the diagnostics pane; on Advanced it
    /// cycles field completions, sampled literals, and then those.
    fn cycle_focus(&mut self, forward: bool, ctx: &mut Ctx<'_>) -> Outcome {
        let Some((view_id, draft)) = self.draft(ctx) else {
            return Outcome::Consumed;
        };
        let target = self.target(&view_id);
        let cursor = ctx.cursors.get_or_end(target.clone(), &draft).char_index;
        let current = match self.focus {
            EditorFocus::Tabs => FocusStop::Tabs,
            EditorFocus::Pane => FocusStop::Pane,
            EditorFocus::Field => self
                .current_completion_kind(&view_id, &draft, &target, cursor)
                .map_or(FocusStop::Field, FocusStop::Completion),
        };
        let stops = self.focus_stops();
        let index = stops.iter().position(|stop| *stop == current).unwrap_or(0);
        let step = if forward { 1 } else { -1 };
        let next = stops[(index as i32 + step).rem_euclid(stops.len() as i32) as usize];
        match next {
            FocusStop::Field => {
                self.focus = EditorFocus::Field;
                self.completion = None;
            }
            FocusStop::Tabs => {
                self.focus = EditorFocus::Tabs;
                self.completion = None;
            }
            FocusStop::Pane => {
                self.focus = EditorFocus::Pane;
                self.completion = None;
            }
            FocusStop::Completion(kind) => {
                self.focus = EditorFocus::Field;
                // The anchored popup needs the painted field rect: reveal the
                // field before offering, so a diagnostic scroll cannot leave
                // the anchor off-screen.
                self.scroll = 0;
                let Some(state) = ctx.views.active() else {
                    return Outcome::Consumed;
                };
                let (items, status) = sample_editor_completion(
                    ctx.provider,
                    &view_id,
                    state.top,
                    state.viewport_height,
                    kind,
                );
                let generation = self.next_completion_generation;
                self.next_completion_generation = generation.saturating_add(1);
                self.completion = Some(EditorCompletionState {
                    generation,
                    view_id,
                    purpose: self.purpose,
                    draft,
                    target,
                    cursor,
                    kind,
                    items,
                    selected: 0,
                    top: 0,
                    status,
                });
            }
        }
        Outcome::Consumed
    }

    fn move_completion(&mut self, delta: i32) {
        if let Some(completion) = &mut self.completion
            && !completion.items.is_empty()
        {
            completion.selected = (completion.selected as i32 + delta)
                .rem_euclid(completion.items.len() as i32)
                as usize;
            completion.top = completion.top.min(completion.selected);
        }
    }

    /// Inserting a completion is fenced against the exact draft and caret it
    /// was offered for; anything else typed since makes it stale and it is
    /// dropped rather than inserted somewhere it no longer fits.
    fn accept_completion(&mut self, ctx: &mut Ctx<'_>) -> Outcome {
        let Some(completion) = self.completion.take() else {
            return Outcome::Consumed;
        };
        let Some((view_id, draft)) = self.draft(ctx) else {
            return Outcome::Consumed;
        };
        if view_id != completion.view_id || self.target(&view_id) != completion.target {
            return Outcome::Consumed;
        }
        let cursor = ctx
            .cursors
            .get_or_end(completion.target.clone(), &draft)
            .char_index;
        if draft != completion.draft || cursor != completion.cursor {
            return Outcome::Consumed;
        }
        if let Some(item) = completion.items.get(completion.selected) {
            let insertion = item.insertion.clone();
            let _ = self.text(EditCommand::Insert(&insertion), ctx);
            if let Some(editor) = ctx.views.editor_mut(&view_id, self.purpose) {
                editor.error = None;
            }
        }
        Outcome::Consumed
    }

    fn scroll_body(&mut self, delta: i32) {
        self.scroll = self
            .scroll
            .saturating_add_signed(delta as isize)
            .min(self.scroll_limit);
    }

    /// The overflow menu drives while it has drawn rows: selection wraps over
    /// the stored overflow list, activation routes through `press_action`
    /// with the ORIGINAL action index so a menu item runs exactly what its
    /// button would.
    fn more_active(&self) -> bool {
        self.more_open && !self.geometry.more_overflow.is_empty()
    }

    fn open_more(&mut self, select: Option<usize>) {
        if self.geometry.more_overflow.is_empty() {
            return;
        }
        self.more_open = true;
        let len = self.geometry.more_overflow.len();
        self.more_selected = select
            .and_then(|index| {
                self.geometry
                    .more_overflow
                    .iter()
                    .position(|&item| item == index)
            })
            .unwrap_or(0)
            .min(len - 1);
        self.more_first = 0;
    }

    fn move_more(&mut self, delta: i32) {
        let len = self.geometry.more_overflow.len();
        if len == 0 {
            return;
        }
        self.more_selected = (self.more_selected as i32 + delta).rem_euclid(len as i32) as usize;
    }

    fn activate_more(&mut self, ctx: &mut Ctx<'_>) -> Outcome {
        let selected = self.more_selected;
        self.more_open = false;
        match self.geometry.more_overflow.get(selected).copied() {
            Some(index) => self.press_action(index, ctx),
            None => Outcome::Consumed,
        }
    }

    /// Arrow keys, which belong to the caret, the popup, the tab control or
    /// the pane depending on what has focus. `App::key_to_action` decided this
    /// from `is_text_editing`/`editor_completion`; the component has both.
    /// An active overflow menu takes the arrows even from the field (it is
    /// mouse-opened without moving focus); otherwise precedence is unchanged.
    fn vertical(&mut self, delta: i32, ctx: &mut Ctx<'_>) -> Outcome {
        if self.text_editing() && self.completion.is_none() && !self.more_active() {
            // In a configured grouping rule Up/Down names the enrichment
            // column instead of moving a caret the single-line token cannot
            // visibly use.
            if self.purpose == QueryPurpose::Grouping
                && let Some((_, draft)) = self.draft(ctx)
                && (draft.starts_with(crate::grouping::RUN_NAMESPACE)
                    || draft.starts_with(crate::grouping::FILTER_NAMESPACE))
            {
                return self.cycle_grouping_column(delta, ctx);
            }
            return self.text(
                if delta < 0 {
                    EditCommand::MoveUp
                } else {
                    EditCommand::MoveDown
                },
                ctx,
            );
        }
        if self.focus == EditorFocus::Pane {
            self.scroll_body(delta);
            return Outcome::Consumed;
        }
        if self.completion.is_some() {
            self.move_completion(delta);
            return Outcome::Consumed;
        }
        if self.more_active() {
            self.move_more(delta);
            return Outcome::Consumed;
        }
        Outcome::Consumed
    }

    fn key(&mut self, key: KeyEvent, ctx: &mut Ctx<'_>) -> Outcome {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return Outcome::Ignored;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return match key.code {
                KeyCode::Char('a') => self.text(EditCommand::StartOfLine, ctx),
                KeyCode::Char('e') => self.text(EditCommand::EndOfLine, ctx),
                KeyCode::Char('k') => self.text(EditCommand::KillToEndOfLine, ctx),
                _ => Outcome::Ignored,
            };
        }
        // The overflow menu is transient: Up/Down/Enter drive it while it is
        // active; any other key dismisses it first and then processes
        // normally, so typing never lands behind an open menu.
        if self.more_open && !matches!(key.code, KeyCode::Up | KeyCode::Down | KeyCode::Enter) {
            self.more_open = false;
        }
        // §8.10 mnemonics (Alt-S, Alt-A, Alt-C, and the bare letters once the
        // field no longer has the keys) never reach here: the shell resolves
        // them against `action_labels` and calls `press_action`.
        match key.code {
            KeyCode::Tab => self.cycle_focus(true, ctx),
            KeyCode::BackTab => self.cycle_focus(false, ctx),
            KeyCode::Up => self.vertical(-1, ctx),
            KeyCode::Down => self.vertical(1, ctx),
            KeyCode::Left if self.focus == EditorFocus::Tabs => self.move_tab(-1, ctx),
            KeyCode::Right if self.focus == EditorFocus::Tabs => self.move_tab(1, ctx),
            KeyCode::Left if self.text_editing() && self.completion.is_none() => {
                self.text(EditCommand::MoveLeft, ctx)
            }
            KeyCode::Right if self.text_editing() && self.completion.is_none() => {
                self.text(EditCommand::MoveRight, ctx)
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::ALT) => {
                self.text(EditCommand::Insert("\n"), ctx)
            }
            // §8.9: a segmented control consumes Enter and Space to select the
            // focused segment, which Left/Right already made the active one.
            KeyCode::Enter | KeyCode::Char(' ') if self.focus == EditorFocus::Tabs => {
                Outcome::Consumed
            }
            KeyCode::Enter => {
                if self.completion.is_some() {
                    self.accept_completion(ctx)
                } else if self.more_active() {
                    self.activate_more(ctx)
                } else {
                    self.submit(ctx)
                }
            }
            KeyCode::Backspace => self.text(EditCommand::Backspace, ctx),
            KeyCode::Char(character) if is_typed_char(&key) => {
                let mut buffer = [0u8; 4];
                self.text(EditCommand::Insert(character.encode_utf8(&mut buffer)), ctx)
            }
            _ => Outcome::Ignored,
        }
    }

    fn mouse(
        &mut self,
        kind: MouseEventKind,
        hit: Option<EditorHit>,
        ctx: &mut Ctx<'_>,
    ) -> Outcome {
        let pressed = matches!(kind, MouseEventKind::Down(MouseButton::Left));
        // Completion owns the mouse only while its popup is actually drawn;
        // state without a painted popup (degenerate layout) falls through to
        // the dialog controls below, mirroring `hit()`.
        if self.completion.is_some() && !self.geometry.completion_popup.is_empty() {
            if pressed
                && let Some(EditorHit::Completion(index)) = hit
                && let Some(completion) = &mut self.completion
            {
                completion.selected = index;
            }
            match kind {
                MouseEventKind::ScrollUp => self.move_completion(-1),
                MouseEventKind::ScrollDown => self.move_completion(1),
                _ => {}
            }
            return Outcome::Consumed;
        }
        // A drawn overflow menu owns the mouse: its rows activate through
        // `press_action`, its button toggles it shut, and anything else
        // dismisses it — the dialog underneath is not clickable through it.
        if !self.geometry.more_rows.is_empty() {
            match (pressed, hit) {
                (true, Some(EditorHit::More(Some(index)))) => {
                    self.more_open = false;
                    return self.press_action(index, ctx);
                }
                (true, Some(EditorHit::More(None))) => {
                    self.more_open = false;
                    return Outcome::Consumed;
                }
                _ if pressed => {
                    self.more_open = false;
                    return Outcome::Consumed;
                }
                _ => return Outcome::Consumed,
            }
        }
        // The menu button toggles the menu open when overflow exists; with no
        // overflow the button is never painted and this arm never fires.
        if pressed && matches!(hit, Some(EditorHit::More(None))) {
            self.open_more(None);
            return Outcome::Consumed;
        }
        if !pressed {
            return Outcome::Consumed;
        }
        // The drawn buttons and segments do exactly what their keys do; the
        // rects come from the same layout that painted them.
        match hit {
            Some(EditorHit::Apply) => self.submit(ctx),
            Some(EditorHit::Clear) => self.clear(ctx),
            Some(EditorHit::Tab(index)) if self.tabbed => FILTER_TABS
                .get(index)
                .map_or(Outcome::Consumed, |purpose| self.switch_tab(*purpose, true)),
            Some(EditorHit::Tab(index)) => self.set_grouping_mode(index, true, ctx),
            _ => Outcome::Consumed,
        }
    }
}

impl Default for EditorDialog {
    fn default() -> Self {
        Self::filter()
    }
}

fn contains(area: Rect, point: (u16, u16)) -> bool {
    point.0 >= area.x && point.0 < area.right() && point.1 >= area.y && point.1 < area.bottom()
}

fn grouping_mode(draft: &str) -> usize {
    // The tab follows intent (namespace), not validity: a half-typed
    // configured rule stays on its tab and the error row reports it.
    if draft.is_empty() {
        3
    } else if draft.starts_with(crate::grouping::RUN_NAMESPACE) {
        0
    } else if draft.starts_with(crate::grouping::FILTER_NAMESPACE) {
        1
    } else {
        // Legacy Auto tokens and custom continuation regexes share the
        // explicit compat tab; their distinct meanings are unchanged. A
        // malformed draft lands here and the error row reports it.
        2
    }
}

/// One line of an applied value for the title: newlines and runs of blanks
/// collapse, because a multi-line Advanced expression is still one predicate.
fn squashed(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The Filter title names every constraint that is applied on the view, so
/// either tab shows both: `Filter · search "seq" · advanced level >= 40`, or
/// `Filter · none`. It reads the same `ViewState` fields the view summary
/// lists, so the two cannot disagree about what is applied.
pub fn filter_title(views: &Views) -> String {
    let mut parts = Vec::new();
    if let Some(state) = views.active() {
        if !state.search.applied.is_empty() {
            parts.push(format!("search \"{}\"", squashed(&state.search.applied)));
        }
        if !state.advanced.applied.is_empty() {
            parts.push(format!("advanced {}", squashed(&state.advanced.applied)));
        }
    }
    if parts.is_empty() {
        "Filter · none".to_owned()
    } else {
        format!("Filter · {}", parts.join(" · "))
    }
}

impl Component for EditorDialog {
    type Hit = EditorHit;
    /// Which tab to open on. `None` keeps the current one; Grouping ignores it.
    type Open = Option<QueryPurpose>;

    fn open(&mut self, tab: Option<QueryPurpose>, ctx: &mut Ctx<'_>) {
        self.open = true;
        self.scroll = 0;
        self.scroll_limit = 0;
        self.focus = EditorFocus::Field;
        self.completion = None;
        self.more_open = false;
        self.more_selected = 0;
        self.more_first = 0;
        self.geometry = EditorGeometry::default();
        if let Some(tab) = tab {
            let _ = self.switch_tab(tab, true);
        }
        // An unset grouping rule opens on the Run tab with a blank column:
        // Run is the primary configured path, and the blank names no column
        // until Up/Down (or typing) picks the enrichment output. Legacy Auto
        // stays reachable under Legacy, never as the default.
        if self.purpose == QueryPurpose::Grouping
            && let Some(view_id) = ctx.views.active_id().map(str::to_owned)
            && let Some(editor) = ctx.views.editor_mut(&view_id, QueryPurpose::Grouping)
            && editor.draft.is_empty()
            && editor.applied.is_empty()
        {
            editor.draft = crate::grouping::run_rule("");
            let caret = editor.draft.clone();
            self.reset_grouping_caret(ctx, &view_id, &caret, true);
        }
    }

    fn handle(&mut self, event: Event<EditorHit>, ctx: &mut Ctx<'_>) -> Outcome {
        match event {
            Event::Key(key) => self.key(key, ctx),
            Event::Paste(text) => self.text(EditCommand::Insert(&text), ctx),
            Event::Mouse { kind, hit, .. } => self.mouse(kind, hit, ctx),
            // §5.3: Escape closes the innermost thing first, so an open
            // completion popup absorbs it rather than the whole dialog.
            Event::Dismiss => {
                if self.completion.take().is_some() {
                    return Outcome::Consumed;
                }
                // §10 frontmost-first: the overflow menu closes before the
                // dialog it hangs from.
                if self.more_open {
                    self.more_open = false;
                    return Outcome::Consumed;
                }
                self.open = false;
                Outcome::Close
            }
            // §4.2: the layer renders its accepted value straight out of
            // `ViewState`, so a completion needs no reaction beyond a redraw.
            // A view whose sources changed underneath it invalidates a popup
            // sampled from the rows that view no longer has.
            Event::View(ViewEvent::SourcesChanged { .. }) => {
                self.completion = None;
                Outcome::Consumed
            }
            Event::Command(CommandId::EditorCompletion) => self.cycle_focus(true, ctx),
            Event::Command(_) | Event::View(_) | Event::Resize => Outcome::Ignored,
        }
    }

    /// §4.3: the Advanced tab offers completion, so the shared catalog row
    /// routes to this layer while that tab is open. Search and Grouping have
    /// nothing to complete and contribute nothing.
    fn commands(&self, _views: &Views) -> Vec<CommandEntry> {
        if !self.tabbed {
            return Vec::new();
        }
        let available = self.open && self.purpose == QueryPurpose::Advanced;
        vec![CommandEntry {
            spec: CommandSpec {
                id: CommandId::EditorCompletion,
                name: "Complete editor field or value",
                description: "Insert a sampled field expression or lexical string without applying",
                category: "Filter",
                aliases: &["autocomplete", "field picker", "sampled value"],
                shortcut: available.then_some("Tab"),
            },
            unavailable_reason: (!available)
                .then_some("open Filter › Advanced or Enrichment first"),
        }]
    }

    /// §8.10: the labels whose underlined letters the shell resolves. Filter
    /// offers its two buttons and its two tabs; Grouping's one button has no
    /// letter, so nothing is ever diverted from its field.
    fn action_labels(&self, _ctx: &Ctx<'_>) -> Vec<&'static str> {
        if self.tabbed {
            FILTER_MNEMONIC_TARGETS.to_vec()
        } else {
            GROUPING_ACTIONS.to_vec()
        }
    }

    /// Press the button or segment at `index` of `action_labels`, as a click
    /// on it would; the focus ring stays where the user is, except that a tab
    /// switch puts the keys in the arriving tab's field. Any press dismisses
    /// the transient overflow menu first — including the menu's own items,
    /// which arrive here with original `FILTER_ACTIONS` indices (0 Apply,
    /// 1 Clear) and run exactly what their buttons would.
    fn press_action(&mut self, index: usize, ctx: &mut Ctx<'_>) -> Outcome {
        self.more_open = false;
        if !self.tabbed {
            return if index == 0 {
                self.submit(ctx)
            } else {
                Outcome::Ignored
            };
        }
        match index {
            0 => self.submit(ctx),
            1 => self.clear(ctx),
            2 => self.switch_tab(QueryPurpose::Search, true),
            3 => self.switch_tab(QueryPurpose::Advanced, true),
            _ => Outcome::Ignored,
        }
    }

    /// Geometry is the last render's, but `text_focus` is not geometry: it is
    /// what decides whether `q` is a character, and a layer that has not been
    /// painted yet still knows the answer. Deriving it here keeps the first
    /// keystroke after `open` from being read as a dismissal.
    fn surface(&self) -> Surface {
        Surface {
            text_focus: self.text_editing() && self.completion.is_none(),
            ..self.surface
        }
    }

    fn hit(&self, point: (u16, u16)) -> Option<EditorHit> {
        let g = &self.geometry;
        // The popup is drawn last and over everything, so it takes the point
        // first — but only when one was actually painted. An open completion
        // whose field had no painted rect draws nothing and claims nothing.
        if self.completion.is_some() && !g.completion_popup.is_empty() {
            return g
                .completion
                .iter()
                .find_map(|(rect, index)| {
                    contains(*rect, point).then_some(EditorHit::Completion(*index))
                })
                .or(Some(EditorHit::Body));
        }
        // A drawn overflow menu sits above the dialog: its rows first, then
        // its button, mirroring paint order. The closed menu's button is
        // hit-tested with the dialog controls below.
        if !g.more_rows.is_empty() {
            return g
                .more_rows
                .iter()
                .find_map(|(rect, index)| {
                    contains(*rect, point).then_some(EditorHit::More(Some(*index)))
                })
                .or_else(|| {
                    g.more_button
                        .filter(|rect| contains(*rect, point))
                        .map(|_| EditorHit::More(None))
                })
                .or(Some(EditorHit::Body));
        }
        if let Some(rect) = g.more_button
            && contains(rect, point)
        {
            return Some(EditorHit::More(None));
        }
        g.tabs
            .iter()
            .position(|rect| contains(*rect, point))
            .map(EditorHit::Tab)
            .or_else(|| {
                g.actions
                    .iter()
                    .position(|rect| contains(*rect, point))
                    .map(|index| {
                        if index == 0 {
                            EditorHit::Apply
                        } else {
                            EditorHit::Clear
                        }
                    })
            })
            .or_else(|| {
                g.diagnostics
                    .filter(|rect| contains(*rect, point))
                    .map(|_| EditorHit::Diagnostics)
            })
            .or_else(|| contains(g.body, point).then_some(EditorHit::Body))
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx<'_>) -> Surface {
        let theme = ctx.theme;
        let ascii = ctx.ascii;
        let Some(view_id) = ctx.views.active_id().map(str::to_owned) else {
            self.surface = Surface::default();
            return self.surface;
        };
        let Some(editor) = ctx.views.editor(&view_id, self.purpose).cloned() else {
            self.surface = Surface::default();
            return self.surface;
        };
        // The caret is the bank's, and it is clamped to the draft it points
        // into, so a draft replaced by a completion can never leave it past
        // the end.
        let cursor = ctx
            .cursors
            .peek(&self.target(&view_id), &editor.draft)
            .unwrap_or_else(|| editor.draft.chars().count());
        let (mut surface, tabs, actions, diagnostics, field) =
            if self.purpose == QueryPurpose::Grouping {
                self.render_grouping(frame, area, &editor, cursor, ctx)
            } else {
                self.render_filter(frame, area, &editor, cursor, ctx)
            };
        // Action labels and roles parallel to the sub-render's action row, in
        // original index order: Filter's Apply/Clear, Grouping's Apply alone.
        // Overflow indices from the shared action geometry name positions in
        // these slices, and activation routes through `press_action`, whose
        // Filter indices 0/1 are the same two verbs.
        let (menu_labels, menu_roles): (&[&str], &[ButtonRole]) =
            if self.purpose == QueryPurpose::Grouping {
                (&GROUPING_ACTIONS, &GROUPING_ROLES)
            } else {
                (&FILTER_ACTIONS, &FILTER_ROLES)
            };
        // Action overflow (`More ▾`) menu, painted before the completion popup
        // so paint order matches hit-test order (completion first). The live
        // overflow list and More button rect were recorded into the geometry
        // by the sub-render from the one shared action geometry; copies below
        // refresh the event-path state between renders.
        let live_overflow = self.geometry.more_overflow.clone();
        let live_more_button = self.geometry.more_button;
        let mut more_rows = Vec::new();
        if self.more_open && !live_overflow.is_empty() {
            if let Some(anchor) = live_more_button {
                let sel = self.more_selected.min(live_overflow.len() - 1);
                let (popup, rows, first, overflows) = draw_action_menu(
                    frame,
                    area,
                    anchor,
                    menu_labels,
                    menu_roles,
                    &live_overflow,
                    sel,
                    self.more_first,
                    theme,
                    ascii,
                );
                self.more_selected = sel;
                self.more_first = first;
                if !popup.is_empty() {
                    surface.popup = surface.popup.union(popup);
                    surface.interior = popup.inner(ratatui::layout::Margin::new(1, 1));
                    surface.scrollable = surface.scrollable || overflows;
                    more_rows = rows;
                }
            }
        } else {
            // Overflow gone (regrew wider) means the menu has nothing to show.
            self.more_open = false;
        }
        self.geometry.more_rows = more_rows;
        // Anchored class A: the completion popup is placed from the actual
        // painted field rect through shared `anchored_geometry` (below
        // preferred with a one-row gap, above when below cannot fit,
        // edge-clamped, max eight rows with scroll and a status footer),
        // bounded by the full render area so it may overhang its parent. One
        // geometry drives its paint, row hitboxes and scrollbar; no centered
        // fallback and no manual popup rect. While a popup is drawn it owns
        // the text selection bound.
        //
        // The shared body scroll reveals the field before completion opens
        // (see `cycle_focus`), so a painted field rect is the norm. When the
        // field has no painted rect — a degenerate layout where the body gave
        // the field no rows — no popup is drawn and no hitboxes are recorded
        // rather than anchoring to a stale or invented rect; the fenced
        // completion state is retained for when the field paints again.
        let mut completion_rows = Vec::new();
        let mut completion_popup = Rect::default();
        let mut completion_overflows = false;
        if let Some(completion) = &self.completion
            && !field.is_empty()
        {
            let (popup, rows, overflows) =
                draw_editor_completion_anchored(frame, area, field, completion, theme, ascii);
            // A refused placement (empty popup: the field's bands hold no
            // gap-honoring popup) leaves the dialog frame, selection bound,
            // caret and wheel interest exactly as the body render left them.
            if !popup.is_empty() {
                completion_popup = popup;
                completion_overflows = overflows;
                surface.popup = surface.popup.union(popup);
                surface.interior = popup.inner(ratatui::layout::Margin::new(1, 1));
                surface.caret = None;
                completion_rows = rows;
            }
        }
        // Wheel interest follows real overflow — the shared body viewport, the
        // diagnostics scroll extent, or the popup list — never an unconditional
        // claim.
        surface.scrollable = surface.scrollable || completion_overflows;
        // `q` is a character only while the field itself has the keys (§1).
        surface.text_focus = self.text_editing() && self.completion.is_none();
        // `frontmost` for modal containment is the union (frame plus any open
        // anchored popup); the shell contains against `Surface.popup`, which we
        // already set to that union.
        self.geometry.tabs = tabs;
        self.geometry.actions = actions;
        self.geometry.diagnostics = diagnostics;
        self.geometry.completion = completion_rows;
        self.geometry.completion_popup = completion_popup;
        self.geometry.field = field;
        self.surface = surface;
        surface
    }
}

impl EditorDialog {
    /// §12.3 multiline grouping: one field, a fixed worked preview, and the
    /// `[ Apply ]` button the dialog anatomy requires.
    ///
    /// `Contextual::Prompt`: top-biased frame from the shared policy plus
    /// stable chrome budgets, never from the preview length, so the frame is
    /// identical across valid/invalid/pending states. The body is the one
    /// shared scroll viewport: logical row 0 is the field, row 1 is the gap,
    /// row 2 is the preview heading and rows 3–4 are the fixed preview lines.
    /// The same projected rects drive paint, caret, selection and mouse; no
    /// independent outer calculation and no component `Clear` beyond the
    /// shared frame renderer.
    fn render_grouping(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        editor: &EditorState,
        cursor: usize,
        ctx: &RenderCtx<'_>,
    ) -> (Surface, Vec<Rect>, Vec<Rect>, Option<Rect>, Rect) {
        let theme = ctx.theme;
        let ascii = ctx.ascii;
        let editor = editor.clone();
        let styles = DialogStyles::new(theme);

        let (state, sentence) = if let Some(error) = editor.error.as_deref() {
            (MessageState::Error, error.to_owned())
        } else if editor.pending_generation.is_some() {
            (
                MessageState::Updating,
                "checking this draft · the last applied grouping stays active".to_owned(),
            )
        } else if editor.applied.is_empty() {
            (
                MessageState::Disabled,
                "an empty draft turns grouping off".to_owned(),
            )
        } else {
            (
                MessageState::Applied,
                crate::grouping::grouping_label(&editor.applied)
                    .unwrap_or("Custom")
                    .to_owned(),
            )
        };
        let auto = editor.draft == crate::grouping::AUTO_GROUPING_TOKEN;
        let run_column = editor
            .draft
            .strip_prefix(crate::grouping::RUN_NAMESPACE)
            .and_then(|suffix| suffix.strip_suffix(')'));
        let filter_column = editor
            .draft
            .strip_prefix(crate::grouping::FILTER_NAMESPACE)
            .and_then(|suffix| suffix.strip_suffix(')'));
        let help = if let Some(column) = run_column {
            if column.is_empty() {
                "Run · Up/Down names the enrichment column whose equal consecutive values form one run."
            } else {
                "Run · equal consecutive values form one run; recognition belongs in Enrichment."
            }
        } else if let Some(column) = filter_column {
            if column.is_empty() {
                "Filter · Up/Down names the enrichment column whose non-null values open events."
            } else {
                "Filter · each non-null value opens an event; everything until the next one continues."
            }
        } else if auto {
            "Legacy Auto · kept for restored settings; prefer Run or Filter. Backspace selects Off."
        } else if editor.draft.is_empty() {
            "Off · grouping is display only; Run and Filter group on enrichment columns."
        } else {
            "Legacy Custom · continuation lines match this regex over raw bytes; prefer Run or Filter."
        };
        let labels = GROUPING_ACTIONS;
        // Fixed logical rows: field, gap, preview heading, two preview lines.
        let content_rows = 5usize;
        let spec = grouping_spec(area);
        let Ok(geometry) = crate::dialog_layout::resolve_dialog(
            area,
            &spec,
            content_rows,
            &labels,
            Some(0),
            ctx.context_anchor,
        ) else {
            // Below the 20x6 floor the tiny fallback owns the frame; stay open
            // with nothing drawn, as the palette does.
            self.geometry.body = Rect::default();
            self.geometry.more_button = None;
            self.geometry.more_overflow = Vec::new();
            self.geometry.more_rows = Vec::new();
            self.geometry.action_band = Rect::default();
            self.scroll_limit = 0;
            return (
                Surface::default(),
                Vec::new(),
                Vec::new(),
                None,
                Rect::default(),
            );
        };
        render_responsive_frame(frame, &geometry, "Multiline grouping", true, theme);
        let mut surface = Surface {
            popup: geometry.frame,
            interior: geometry.interior,
            caret: None,
            scrollable: geometry.body.overflow() > 0,
            text_focus: self.text_editing() && self.completion.is_none(),
        };
        self.geometry.body = geometry.body.viewport;
        self.geometry.action_band = geometry.actions.band;
        let active_mode = grouping_mode(&editor.draft);
        let focused_mode = (self.focus == EditorFocus::Tabs).then_some(active_mode);
        let modes = render_segmented_control(
            frame,
            geometry.header,
            &GROUPING_MODE_LABELS,
            active_mode,
            focused_mode,
            theme,
        );
        // Shared body projection: the same rects drive paint, caret and mouse.
        // The preview is fixed content, so the scroll window stays at the top;
        // a tight viewport scrolls the body under its scrollbar instead of
        // clipping the field away.
        let mut body = ScrollViewport::new(geometry.body.viewport, content_rows, 0);
        // Reveal the focused control when Tab reaches the mode control? The
        // field (row 0) and preview (rows 2–4) share one viewport; the mode
        // control lives in the sticky header, never in the body, so no reveal
        // is needed beyond keeping row 0 visible while editing.
        if self.text_editing() {
            body = ScrollViewport::new(geometry.body.viewport, content_rows, body.reveal(0));
        }
        self.scroll_limit = body.overflow();
        if self.focus == EditorFocus::Pane && self.scroll_limit == 0 {
            self.focus = EditorFocus::Field;
        }
        let mut field_rect = Rect::default();
        if let Some(field) = body.project_row(0) {
            field_rect = field;
            if auto {
                InputSurface {
                    style: styles.input,
                }
                .render(field, frame.buffer_mut());
                frame.render_widget(
                    Paragraph::new("Auto — conservative multiline detection").style(styles.input),
                    field,
                );
            } else if self.text_editing() {
                surface.caret =
                    place_input_cursor_at(frame, field, 0, 0, &editor.draft, cursor, theme);
            } else {
                InputSurface {
                    style: styles.input,
                }
                .render(field, frame.buffer_mut());
                frame.render_widget(
                    Paragraph::new(truncated(&editor.draft, usize::from(field.width)))
                        .style(styles.input),
                    field,
                );
            }
        }
        // Row 1 is the §4.1 gap between the field and the preview pane; row 2
        // is the pane heading, rows 3–4 the fixed preview lines.
        if let Some(heading) = body.project_row(2) {
            frame.render_widget(
                Paragraph::new("Preview").style(styles.label.add_modifier(Modifier::BOLD)),
                heading,
            );
        }
        for (index, line) in GROUPING_PREVIEW.iter().enumerate() {
            if let Some(row) = body.project_row(3 + index) {
                frame.render_widget(
                    Paragraph::new(truncated(line, usize::from(row.width)))
                        .style(styles.description),
                    row,
                );
            }
        }
        if let Some(bar) = body.scrollbar {
            render_scrollbar(frame, bar, body.first_row, body.overflow(), theme, ascii);
        }
        // The status is one line now, so nothing overflows and no scroll
        // affordance is claimed (§9).
        let diagnostics = None;

        render_message(frame, geometry.message, state, &sentence, theme, ascii);
        render_help_text(frame, geometry.help, help, theme);
        // One shared action geometry drives paint and hitboxes: the filled
        // default (§8.9) plus the drawn rects the mouse handler is given, so
        // click and paint cannot disagree. The live overflow list and More
        // button rect are recorded for the menu block in `render` and for
        // event paths between renders; the More ring shows while its menu is
        // open (actions are never Tab stops here, so it owns no focus).
        self.geometry.more_overflow = geometry.actions.overflow.clone();
        self.geometry.more_button = geometry.actions.more;
        let mut actions = vec![Rect::default(); labels.len()];
        for (index, rect) in &geometry.actions.buttons {
            if let Some(slot) = actions.get_mut(*index) {
                *slot = *rect;
            }
            if let Some(label) = labels.get(*index) {
                let role = if Some(*index) == geometry.actions.default {
                    crate::dialog_controls::ButtonRole::Default
                } else {
                    crate::dialog_controls::ButtonRole::Normal
                };
                render_role_button(frame, *rect, label, role, false, theme);
            }
        }
        if let Some(more) = geometry.actions.more {
            render_role_button(
                frame,
                more,
                crate::dialog_controls::MORE_LABEL,
                crate::dialog_controls::ButtonRole::Normal,
                self.more_open,
                theme,
            );
        }
        (surface, modes, actions, diagnostics, field_rect)
    }

    /// §12.1 Filter: the `Search │ Advanced` control, one field, one message
    /// row, one overflowing-diagnostic pane and `[ Apply ] [ Clear ]`; the two
    /// tabs differ only in their words and in the completion Advanced offers.
    ///
    /// `Contextual::Prompt`: top-biased frame from the shared policy plus
    /// stable chrome budgets, never from the diagnostic length, so the frame is
    /// identical across valid/invalid/pending/diagnostic states. The body is
    /// the one shared scroll viewport: logical row 0 is the field, row 1 is the
    /// diagnostics heading when the message overflows, and the following rows
    /// are the full wrapped diagnostic text. The same projected rects drive
    /// paint, caret, scrollbar and mouse; no independent outer calculation.
    fn render_filter(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        editor: &EditorState,
        cursor: usize,
        ctx: &RenderCtx<'_>,
    ) -> (Surface, Vec<Rect>, Vec<Rect>, Option<Rect>, Rect) {
        let theme = ctx.theme;
        let ascii = ctx.ascii;
        let editor = editor.clone();
        let styles = DialogStyles::new(theme);
        let search = self.purpose == QueryPurpose::Search;
        let help = if search {
            r#"Examples: text · "field name": text · /regex/ims · \/literal"#
        } else {
            "Use a Polars expression. Fields and sampled literals complete with Tab."
        };

        // §7.4: one message row. The last accepted value stays in the sentence, so
        // a failing draft never hides the filter that is actually applied.
        let (state, mut sentence) = if let Some(error) = editor.error.as_deref() {
            (MessageState::Error, error.to_owned())
        } else if editor.pending_generation.is_some() {
            (
                MessageState::Updating,
                "checking this draft · the last applied view stays visible".to_owned(),
            )
        } else if editor.applied.is_empty() {
            (MessageState::NoFilter, "every record is shown".to_owned())
        } else {
            (MessageState::Applied, editor.applied.clone())
        };
        if !editor.applied.is_empty() && editor.draft != editor.applied {
            sentence.push_str(" · last accepted ");
            sentence.push_str(&editor.applied);
        }

        // Natural diagnostic rows for the scroll extent (not the frame): when
        // the sentence does not fit the two-row message cap, the full text
        // becomes scrollable body content so it stays reachable.
        let (policy_width, _) = crate::dialog_layout::policy_size(
            area,
            PresentationKind::Contextual(ContextFootprint::Prompt),
        );
        let measure_width = usize::from(policy_width.saturating_sub(4)).max(1);
        let message_width = measure_width
            .saturating_sub(usize::from(MESSAGE_SENTENCE_COLUMN))
            .max(1);
        let wrapped = wrap_sentence(&sentence, message_width, usize::MAX);
        // Overflow against the stable two-row cap, measured at this viewport's
        // policy width so the scroll extent matches what is painted.
        let cap_rows = 2usize;
        let overflows = wrapped.len() > cap_rows;
        let content_rows = 1usize.saturating_add(if overflows {
            1usize.saturating_add(wrapped.len())
        } else {
            0
        });

        let labels = FILTER_ACTIONS;
        let spec = filter_spec(area);
        // The title says what is applied, on both tabs. Truncated to the
        // resolved content width below, once the shared frame exists.
        let Ok(geometry) = crate::dialog_layout::resolve_dialog(
            area,
            &spec,
            content_rows,
            &labels,
            Some(0),
            ctx.context_anchor,
        ) else {
            self.geometry.body = Rect::default();
            self.geometry.more_button = None;
            self.geometry.more_overflow = Vec::new();
            self.geometry.more_rows = Vec::new();
            self.geometry.action_band = Rect::default();
            self.scroll_limit = 0;
            return (
                Surface::default(),
                Vec::new(),
                Vec::new(),
                None,
                Rect::default(),
            );
        };
        let title = truncated(
            &filter_title(ctx.views),
            usize::from(geometry.content.width).max(1),
        );
        render_responsive_frame(frame, &geometry, &title, true, theme);
        let mut surface = Surface {
            popup: geometry.frame,
            interior: geometry.interior,
            caret: None,
            scrollable: geometry.body.overflow() > 0 || overflows,
            text_focus: self.text_editing() && self.completion.is_none(),
        };
        self.geometry.body = geometry.body.viewport;
        self.geometry.action_band = geometry.actions.band;

        // §8.6: the tab control is the header. The active segment carries the
        // selection style; the focus ring underlines it when Tab reaches it.
        let active = FILTER_TABS
            .iter()
            .position(|tab| *tab == self.purpose)
            .unwrap_or(0);
        let focused = (self.focus == EditorFocus::Tabs).then_some(active);
        let tabs = render_segmented_control(
            frame,
            geometry.header,
            &FILTER_TAB_LABELS,
            active,
            focused,
            theme,
        );

        // Shared body projection: row 0 is the field; when the message
        // overflows, row 1 is the diagnostics heading and rows 2.. the full
        // wrapped text. The scroll offset is the shared viewport's first row,
        // clamped into range; the same rects drive paint, scrollbar and mouse.
        let mut body = ScrollViewport::new(geometry.body.viewport, content_rows, self.scroll);
        // Keep the field visible while editing; the diagnostics pane scrolls
        // under it when focused.
        if self.text_editing() && self.completion.is_none() {
            // Do not force row 0 when the user deliberately scrolled the pane:
            // only reveal when the field is the focus and the pane is not.
            if self.focus != EditorFocus::Pane {
                body = ScrollViewport::new(geometry.body.viewport, content_rows, body.reveal(0));
            }
        }
        self.scroll = body.first_row;
        self.scroll_limit = body.overflow();
        let mut diagnostics: Option<Rect> = None;
        // The pane stop exists only while there is a pane; a pane that just
        // went away hands the keys back to the field.
        if self.focus == EditorFocus::Pane && self.scroll_limit == 0 && !overflows {
            self.focus = EditorFocus::Field;
        }

        let mut field_rect = Rect::default();
        if let Some(field) = body.project_row(0) {
            field_rect = field;
            if self.completion.is_none() && self.text_editing() {
                surface.caret =
                    place_input_cursor_at(frame, field, 0, 0, &editor.draft, cursor, theme);
                if editor.draft.is_empty() {
                    let placeholder = if search {
                        "Type to filter…"
                    } else {
                        r#"Polars expression, e.g. col("level") == "ERROR""#
                    };
                    render_placeholder(
                        frame,
                        Rect::new(
                            field.x.saturating_add(1),
                            field.y,
                            field.width.saturating_sub(1),
                            1,
                        ),
                        placeholder,
                        theme,
                    );
                }
            } else {
                InputSurface {
                    style: styles.input,
                }
                .render(field, frame.buffer_mut());
                frame.render_widget(
                    Paragraph::new(truncated(&editor.draft, usize::from(field.width)))
                        .style(styles.input),
                    field,
                );
            }
        }

        if overflows {
            if let Some(heading) = body.project_row(1) {
                frame.render_widget(
                    Paragraph::new("Diagnostics").style(styles.label.add_modifier(Modifier::BOLD)),
                    heading,
                );
            }
            for (index, line) in wrapped.iter().enumerate() {
                if let Some(row) = body.project_row(2 + index) {
                    frame
                        .render_widget(Paragraph::new(line.clone()).style(styles.description), row);
                }
            }
            // The diagnostics affordance is the shared body scrollbar plus the
            // viewport itself for hit-testing; the heading row above names it.
            if body.overflow() > 0 {
                diagnostics = Some(body.viewport);
            }
            if let Some(bar) = body.scrollbar {
                render_scrollbar(frame, bar, body.first_row, body.overflow(), theme, ascii);
            }
        } else {
            // A status that fits claims no affordance (§9).
            diagnostics = None;
        }

        render_message(frame, geometry.message, state, &sentence, theme, ascii);
        render_help_text(frame, geometry.help, help, theme);
        // §8.9: Apply is the default and is filled; the hitboxes are the rects
        // the shared action geometry drew, so click and paint cannot disagree.
        // The live overflow list and More button rect are recorded for the menu
        // block in `render` and for event paths between renders.
        self.geometry.more_overflow = geometry.actions.overflow.clone();
        self.geometry.more_button = geometry.actions.more;
        let mut actions = vec![Rect::default(); labels.len()];
        for (index, rect) in &geometry.actions.buttons {
            if let Some(slot) = actions.get_mut(*index) {
                *slot = *rect;
            }
            if let Some(label) = labels.get(*index) {
                let role = if Some(*index) == geometry.actions.default {
                    crate::dialog_controls::ButtonRole::Default
                } else {
                    crate::dialog_controls::ButtonRole::Normal
                };
                render_role_button(frame, *rect, label, role, false, theme);
            }
        }
        if let Some(more) = geometry.actions.more {
            render_role_button(
                frame,
                more,
                crate::dialog_controls::MORE_LABEL,
                crate::dialog_controls::ButtonRole::Normal,
                self.more_open,
                theme,
            );
        }
        (surface, tabs, actions, diagnostics, field_rect)
    }
}
