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
use ratatui::{Frame, layout::Rect, style::Modifier, widgets::Paragraph};

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
use crate::text_edit::{EditCommand, EditPolicy, TextTarget, edit};
use crate::ui::{
    InputSurface, MESSAGE_SENTENCE_COLUMN, MessageState, dialog_frame_regions,
    draw_editor_completion, help_rows, message_rows, packed_button_rows, place_input_cursor_at,
    render_action_row, render_help_text, render_message, render_placeholder, render_scrollbar,
    render_segmented_control, truncated, wrap_sentence,
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

/// Everything an editor draws that can be clicked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EditorHit {
    /// A row of the completion popup.
    Completion(usize),
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
    completion: Vec<(Rect, usize)>,
    tabs: Vec<Rect>,
    /// Button rects in `FILTER_ACTIONS` order (Grouping draws only `Apply`).
    actions: Vec<Rect>,
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

    /// Arrow keys, which belong to the caret, the popup, the tab control or
    /// the pane depending on what has focus. `App::key_to_action` decided this
    /// from `is_text_editing`/`editor_completion`; the component has both.
    fn vertical(&mut self, delta: i32, ctx: &mut Ctx<'_>) -> Outcome {
        if self.text_editing() && self.completion.is_none() {
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
        if self.completion.is_some() {
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
    /// switch puts the keys in the arriving tab's field.
    fn press_action(&mut self, index: usize, ctx: &mut Ctx<'_>) -> Outcome {
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
        // first — and while it is open nothing under it is clickable.
        if self.completion.is_some() {
            return g
                .completion
                .iter()
                .find_map(|(rect, index)| {
                    contains(*rect, point).then_some(EditorHit::Completion(*index))
                })
                .or(Some(EditorHit::Body));
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
        let (mut surface, tabs, actions, diagnostics) = if self.purpose == QueryPurpose::Grouping {
            self.render_grouping(frame, area, &editor, cursor, ctx)
        } else {
            self.render_filter(frame, area, &editor, cursor, ctx)
        };
        // §5.2: the popup is part of what this layer drew, so containment is
        // against the union; and while it is open it owns the text selection
        // bound, exactly as it owned `selection_modal` before.
        let mut completion_rows = Vec::new();
        if let Some(completion) = &self.completion {
            let (popup, rows) = draw_editor_completion(frame, area, completion, theme);
            surface.popup = surface.popup.union(popup);
            surface.interior = popup.inner(ratatui::layout::Margin::new(1, 1));
            surface.caret = None;
            completion_rows = rows;
        }
        surface.scrollable = true;
        // `q` is a character only while the field itself has the keys (§1).
        surface.text_focus = self.text_editing() && self.completion.is_none();
        self.geometry.tabs = tabs;
        self.geometry.actions = actions;
        self.geometry.diagnostics = diagnostics;
        self.geometry.completion = completion_rows;
        self.surface = surface;
        surface
    }
}

impl EditorDialog {
    /// §12.3 multiline grouping: one field, a fixed worked preview, and the
    /// `[ Apply ]` button the dialog anatomy requires.
    fn render_grouping(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        editor: &EditorState,
        cursor: usize,
        ctx: &RenderCtx<'_>,
    ) -> (Surface, Vec<Rect>, Vec<Rect>, Option<Rect>) {
        let theme = ctx.theme;
        let mut surface = Surface::default();
        let mut actions: Vec<Rect> = Vec::new();
        let mut diagnostics: Option<Rect> = None;
        let editor = editor.clone();
        use crate::dialog_layout::{DialogClass, DialogContent, content_width, pane};

        let styles = DialogStyles::new(theme);
        let ascii = ctx.ascii;
        let width = content_width(area, DialogClass::S);

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
        // §3: the action row is part of the anatomy, not an afterthought. Without
        // it this dialog rendered no way to apply at all and relied on the user
        // knowing that Enter works.
        let labels = GROUPING_ACTIONS;

        let preview_rows = u16::try_from(GROUPING_PREVIEW.len()).unwrap_or(2);
        let content = DialogContent {
            header: 1,
            // input row, gap, preview pane heading, preview rows
            body: 3u16.saturating_add(preview_rows),
            message: message_rows(&sentence, width),
            help: help_rows(help, width),
            actions: packed_button_rows(width, &labels),
        };
        let regions = dialog_frame_regions(
            frame,
            area,
            DialogClass::S,
            "Multiline grouping",
            &content,
            theme,
        );
        surface.popup = regions.popup;
        surface.interior = regions.interior;
        self.geometry.body = regions.body;
        let active_mode = grouping_mode(&editor.draft);
        let focused_mode = (self.focus == EditorFocus::Tabs).then_some(active_mode);
        let modes = render_segmented_control(
            frame,
            regions.header,
            &GROUPING_MODE_LABELS,
            active_mode,
            focused_mode,
            theme,
        );
        if regions.body.width == 0 || regions.body.height == 0 {
            return (surface, modes, actions, diagnostics);
        }

        let field = Rect::new(regions.body.x, regions.body.y, regions.body.width, 1);
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
            surface.caret = place_input_cursor_at(frame, field, 0, 0, &editor.draft, cursor, theme);
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

        // §8.7: the preview is a pane, not three loose rows under a colon label.
        let preview_area = Rect::new(
            regions.body.x,
            regions.body.y.saturating_add(2),
            regions.body.width,
            regions.body.height.saturating_sub(2),
        );
        if preview_area.height > 0 {
            let rects = pane(preview_area, 0, GROUPING_PREVIEW.len());
            if rects.heading.height > 0 {
                frame.render_widget(
                    Paragraph::new("Preview").style(styles.label.add_modifier(Modifier::BOLD)),
                    rects.heading,
                );
            }
            for (index, line) in GROUPING_PREVIEW.iter().enumerate() {
                let Some(y) = u16::try_from(index)
                    .ok()
                    .map(|offset| rects.viewport.y.saturating_add(offset))
                    .filter(|y| *y < rects.viewport.bottom())
                else {
                    continue;
                };
                frame.render_widget(
                    Paragraph::new(truncated(line, usize::from(rects.viewport.width)))
                        .style(styles.description),
                    Rect::new(rects.viewport.x, y, rects.viewport.width, 1),
                );
            }
        }

        // The status is one line now, so nothing overflows and no scroll
        // affordance is claimed (§9).
        self.scroll_limit = 0;
        diagnostics = None;

        render_message(frame, regions.message, state, &sentence, theme, ascii);
        render_help_text(frame, regions.help, help, theme);
        // The hitbox is the rect the button was drawn into, so click and paint can
        // never disagree.
        actions = render_action_row(frame, regions.actions, &labels, None, &[], theme)
            .into_iter()
            .map(|(_, rect)| rect)
            .collect();
        (surface, modes, actions, diagnostics)
    }

    /// §12.1 Filter: the `Search │ Advanced` control, one field, one message
    /// row, one overflowing-diagnostic pane and `[ Apply ] [ Clear ]`; the two
    /// tabs differ only in their words and in the completion Advanced offers.
    fn render_filter(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        editor: &EditorState,
        cursor: usize,
        ctx: &RenderCtx<'_>,
    ) -> (Surface, Vec<Rect>, Vec<Rect>, Option<Rect>) {
        let theme = ctx.theme;
        let mut surface = Surface::default();
        let mut diagnostics: Option<Rect> = None;
        let editor = editor.clone();
        use crate::dialog_layout::{DialogClass, DialogContent, content_width};

        let styles = DialogStyles::new(theme);
        let ascii = ctx.ascii;
        let search = self.purpose == QueryPurpose::Search;
        let help = if search {
            r#"Examples: text · "field name": text · /regex/ims · \/literal"#
        } else {
            "Use a Polars expression. Fields and sampled literals complete with Tab."
        };
        let width = content_width(area, DialogClass::S);
        // The title says what is applied, on both tabs, and is cut to the
        // frame rather than wrapping the border.
        let title = truncated(&filter_title(ctx.views), usize::from(width));

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

        // §7.4 caps the message row at two rows, but a rejected expression can carry
        // a long diagnostic and AGENTS.md requires it to stay reachable. When the
        // sentence does not fit, the state stays in the message row and the full
        // text moves into a scrollable pane (§9) instead of being clipped away.
        let message_width = usize::from(width.saturating_sub(MESSAGE_SENTENCE_COLUMN)).max(1);
        let wrapped = wrap_sentence(&sentence, message_width, usize::MAX);
        let overflows = wrapped.len() > usize::from(message_rows(&sentence, width));
        let diagnostic_rows = if overflows {
            u16::try_from(wrapped.len()).unwrap_or(u16::MAX).min(8)
        } else {
            0
        };

        let content = DialogContent {
            header: 1,
            body: if overflows {
                2u16.saturating_add(diagnostic_rows)
            } else {
                1
            },
            message: message_rows(&sentence, width),
            help: help_rows(help, width),
            actions: packed_button_rows(width, &FILTER_ACTIONS),
        };
        let regions = dialog_frame_regions(frame, area, DialogClass::S, &title, &content, theme);
        surface.popup = regions.popup;
        surface.interior = regions.interior;
        self.geometry.body = regions.body;

        // §8.6: the tab control is the header. The active segment carries the
        // selection style; the focus ring underlines it when Tab reaches it.
        let active = FILTER_TABS
            .iter()
            .position(|tab| *tab == self.purpose)
            .unwrap_or(0);
        let focused = (self.focus == EditorFocus::Tabs).then_some(active);
        let tabs = render_segmented_control(
            frame,
            regions.header,
            &FILTER_TAB_LABELS,
            active,
            focused,
            theme,
        );

        if regions.body.width == 0 || regions.body.height == 0 {
            return (surface, tabs, Vec::new(), diagnostics);
        }

        let field = Rect::new(regions.body.x, regions.body.y, regions.body.width, 1);
        if self.completion.is_none() && self.text_editing() {
            surface.caret = place_input_cursor_at(frame, field, 0, 0, &editor.draft, cursor, theme);
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

        if overflows && regions.body.height > 1 {
            let pane_area = Rect::new(
                regions.body.x,
                regions.body.y.saturating_add(1),
                regions.body.width,
                regions.body.height.saturating_sub(1),
            );
            let rects = crate::dialog_layout::pane(pane_area, 0, wrapped.len());
            if rects.heading.height > 0 {
                frame.render_widget(
                    Paragraph::new("Diagnostics").style(styles.label.add_modifier(Modifier::BOLD)),
                    rects.heading,
                );
            }
            let visible = usize::from(rects.viewport.height);
            let limit = wrapped.len().saturating_sub(visible);
            self.scroll_limit = limit;
            self.scroll = self.scroll.min(limit);
            diagnostics = (limit > 0).then_some(rects.viewport);
            for (offset, line) in wrapped.iter().skip(self.scroll).take(visible).enumerate() {
                frame.render_widget(
                    Paragraph::new(line.clone()).style(styles.description),
                    Rect::new(
                        rects.viewport.x,
                        rects.viewport.y.saturating_add(offset as u16),
                        rects.viewport.width,
                        1,
                    ),
                );
            }
            if let Some(bar) = rects.scrollbar {
                render_scrollbar(frame, bar, self.scroll, limit, theme, ascii);
            }
        } else {
            // A status that fits claims no affordance (§9).
            self.scroll_limit = 0;
            diagnostics = None;
        }
        // The pane stop exists only while there is a pane; a pane that just
        // went away hands the keys back to the field.
        if self.focus == EditorFocus::Pane && self.scroll_limit == 0 {
            self.focus = EditorFocus::Field;
        }

        render_message(frame, regions.message, state, &sentence, theme, ascii);
        render_help_text(frame, regions.help, help, theme);
        // §8.9: Apply is the default and is filled; the hitboxes are the rects
        // the buttons were drawn into, so click and paint cannot disagree.
        let actions = render_action_row(frame, regions.actions, &FILTER_ACTIONS, None, &[], theme)
            .into_iter()
            .map(|(_, rect)| rect)
            .collect();
        (surface, tabs, actions, diagnostics)
    }
}
