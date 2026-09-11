//! The Colour rules layer (`c`): an ordered, per-view list of
//! "when <match> then <colour>" rules that decide how a row is painted.
//!
//! Everything it edits is view-owned (§2.5). `ViewState.color_rules` is the
//! accepted list — what the rows on screen were painted with — and
//! `color_rules_draft` is the working list the dialog edits, kept per view so
//! closing and reopening resumes the edit and a restart restores it. Nothing is
//! cached here; the layer renders from `ctx.views.active()` every frame.
//!
//! A rule is either a column classification or a legacy predicate. Column
//! rules name an accepted enrichment output and an exact value: patterns and
//! keys belong in ordinary enrichment definitions, so colour only reads their
//! outputs — a new rule never starts life as an independent raw pattern
//! classifier. Raw literal text remains as the explicit exception for views
//! with nothing to classify yet. Legacy predicate rules restored from earlier
//! versions keep working unchanged; the dialog no longer offers their syntax.
//!
//! Two things this layer deliberately does not do:
//!
//! * **It does not evaluate a match.** A legacy predicate is written in the
//!   search box's own language and is compiled and run by the query engine,
//!   in the same batch pass and through the same `TextSearch` the filter
//!   uses; a column rule is an exact lookup of the worker's ready derived
//!   cell. What this dialog validates is only what can be checked without
//!   data — non-empty text, the byte cap, compilable `/regex/` for legacy
//!   predicates, and a named column plus value for column rules. Everything
//!   else is the engine's answer.
//! * **It does not narrow the view.** Rules are presentation. Applying them
//!   submits one query, exactly as display-only grouping does, and the last
//!   applied view stays on screen while it settles.

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::{Frame, layout::Rect, style::Modifier, widgets::Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::app::{ColorRule, MAX_COLOR_RULES, RuleColor, SubmitRefused, Views};
use crate::command_palette::CommandId;
use crate::component::{
    CommandEntry, CommandSpec, Component, Ctx, Event, Outcome, RenderCtx, Surface, ViewEvent,
};
use crate::components::editors::draw_action_menu;
use crate::dialog_controls::DialogStyles;
use crate::dialog_controls::{ButtonRole, render_role_button};
use crate::dialog_layout::{ContextFootprint, DialogSpec, PresentationKind, ScrollViewport};
use crate::text_edit::{EditCommand, EditPolicy, TextCursor, edit, reset_cursor_to_end};
use crate::ui::{
    FIELD_GUTTER, MessageState, clipped_width, place_input_cursor_at, render_help_text,
    render_message, render_responsive_frame, render_scrollbar, truncated,
};

/// A predicate is a filter expression, not a document.
const MAX_PREDICATE_BYTES: usize = 512;

/// Which control has focus. UI-only state, so it lives here (§3).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ColorRulesControl {
    /// The rule list.
    #[default]
    List,
    /// The enrichment column a column rule classifies. Reached by Tab only
    /// when the selected rule is a column rule.
    Column,
    /// The text field for the rule being added or edited: the exact value
    /// for a column rule, the predicate for a legacy rule.
    Predicate,
    /// The colour chooser for that rule.
    Color,
    Add,
    Remove,
    Apply,
}

/// Everything the dialog draws that can be clicked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ColorRulesHit {
    Row(usize),
    Swatch(usize),
    /// The `More ▾` button (`None`) or a row of its open overflow menu, by
    /// ORIGINAL action index (0 Add, 1 Remove, 2 Apply) for `press_action`.
    More(Option<usize>),
    Control(ColorRulesControl),
    Body,
}

#[derive(Clone, Debug, Default)]
struct ColorRulesGeometry {
    body: Rect,
    rows: Vec<(Rect, usize)>,
    swatches: Vec<(Rect, usize)>,
    controls: Vec<(Rect, ColorRulesControl)>,
    /// The painted `More ▾` button rect, if the shared action geometry planned
    /// one this frame.
    more_button: Option<Rect>,
    /// Overflow action indices hidden behind `More ▾` this frame, in display
    /// order. Event paths read this copy; paint reads the live geometry.
    more_overflow: Vec<usize>,
    /// Painted overflow-menu rows with ORIGINAL action indices. Empty unless
    /// the menu painted.
    more_rows: Vec<(Rect, usize)>,
    /// The resolved sticky action band (kept rows after pressure).
    action_band: Rect,
}

#[derive(Debug, Default)]
pub struct ColorRulesDialog {
    open: bool,
    selected: usize,
    control: ColorRulesControl,
    /// The predicate being typed, and its caret. Dialog-owned rather than
    /// view-owned: it is one row of the draft list being edited in place, and
    /// it is committed into that list on every keystroke, so nothing is lost.
    cursor: TextCursor,
    /// Set while a newly added rule has never been committed, so leaving the
    /// field empty removes it again instead of leaving a rule that matches
    /// nothing.
    adding: bool,
    top: usize,
    /// The action-overflow (`More ▾`) menu: open flag, selection as a position
    /// within the overflow list, and retained scroll offset. Activation routes
    /// through `press_action` with original action indices.
    more_open: bool,
    more_selected: usize,
    more_first: usize,
    geometry: ColorRulesGeometry,
    surface: Surface,
}

/// The action row, in drawn order, with its §8.10 mnemonics. `Add` takes the
/// `A`, so `Apply` underlines its `p`; the shell resolves both from these
/// labels, which is why the dialog keeps no Alt keymap of its own.
const COLOR_RULES_BUTTONS: [&str; 3] = ["&Add", "&Remove", "A&pply"];

/// Stable responsive budgets for the Colour rules inspector.
///
/// `Contextual::Inspector` against the frozen opening-row anchor: the frame
/// avoids the referent row when possible (above/below with a one-row gap,
/// log-centered, shrunk into the larger band, top-biased fallback) and never
/// chases live selection. Outer size comes from the presentation policy plus
/// these stable maxima alone — never from the rule count or the message
/// length — so empty/populated/pending/error frames share one `frame` and
/// sticky tail origins. `body_content_rows` sizes only the shared scroll
/// extent; the list, every editor row and the caret stay reachable through
/// shared body projection and focus reveal.
///
/// Action/message/help budgets are area-aware (see `responsive_chrome`): one
/// action row at roomy widths, two where the verbs wrap, with message/help
/// pre-shed to their floor at short heights so the two-row band survives
/// degradation with full-width controls.
///
/// Hand-rolled budget choice here is presentation-only folding, never query
/// membership (AGENTS.md).
fn color_spec(area: Rect) -> DialogSpec {
    let (message, help, actions) = crate::components::editors::responsive_chrome(
        area,
        PresentationKind::Contextual(ContextFootprint::Inspector),
        0,
        2,
        2,
        1,
        &COLOR_RULES_BUTTONS,
    );
    DialogSpec::new(
        PresentationKind::Contextual(ContextFootprint::Inspector),
        0,
        1,
        message,
        help,
        actions,
    )
}

impl ColorRulesDialog {
    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn control(&self) -> ColorRulesControl {
        self.control
    }

    pub fn row_rects(&self) -> &[(Rect, usize)] {
        &self.geometry.rows
    }

    /// The painted `More ▾` button rect, if overflow planned one this frame.
    /// Tests use this to assert no invented overflow at roomy sizes.
    pub fn more_button(&self) -> Option<Rect> {
        self.geometry.more_button
    }

    /// Painted overflow-menu rows with original action indices. Empty unless
    /// the menu painted.
    pub fn more_rows(&self) -> &[(Rect, usize)] {
        &self.geometry.more_rows
    }

    /// Whether the overflow menu is open (state, independent of paint).
    pub fn more_open(&self) -> bool {
        self.more_open
    }

    /// The resolved sticky action band (kept rows after pressure). Tests
    /// assert its height equals the requested budget.
    pub fn action_band(&self) -> Rect {
        self.geometry.action_band
    }

    pub fn control_rects(&self) -> &[(Rect, ColorRulesControl)] {
        &self.geometry.controls
    }

    fn draft(ctx: &Ctx<'_>) -> Vec<ColorRule> {
        ctx.views
            .active()
            .map(|state| state.color_rules_draft.clone())
            .unwrap_or_default()
    }

    fn with_draft(ctx: &mut Ctx<'_>, edit: impl FnOnce(&mut Vec<ColorRule>)) {
        let Some(view_id) = ctx.views.active_id().map(str::to_owned) else {
            return;
        };
        if let Some(state) = ctx.views.state_mut(&view_id) {
            edit(&mut state.color_rules_draft);
            state.color_rules_error = None;
        }
        ctx.views.touch(&view_id);
    }

    fn error(ctx: &mut Ctx<'_>, message: &str) {
        if let Some(state) = ctx.views.active_mut() {
            state.color_rules_error = Some(message.to_owned());
        }
    }

    /// Accepted enrichment outputs a new or repointed rule may classify: the
    /// accepted membership's declared output inventory, read through the
    /// provider rather than by parsing definition source — which is blind to
    /// slash-shorthand named captures — and independent of which rows (if
    /// any) are currently served, so a pending page or a settled zero-row
    /// filter never forces the raw-text exception. Sorted and deduplicated
    /// for the chooser; empty only when the accepted chain declares nothing.
    fn classifiable_columns(ctx: &Ctx<'_>) -> Vec<String> {
        let Some(view_id) = ctx.views.active_id() else {
            return Vec::new();
        };
        let mut columns = ctx.provider.enrichment_outputs(view_id);
        columns.sort();
        columns.dedup();
        columns
    }

    /// Whether the selected draft rule classifies a column. The text field
    /// edits its exact value; otherwise it edits a legacy predicate.
    fn selected_is_column(ctx: &Ctx<'_>, selected: usize) -> bool {
        Self::draft(ctx)
            .get(selected)
            .is_some_and(ColorRule::is_column)
    }

    /// The text the field edits for the selected rule: exact value or
    /// legacy predicate.
    fn selected_text(ctx: &Ctx<'_>, selected: usize) -> String {
        Self::draft(ctx)
            .get(selected)
            .map(|rule| {
                if rule.is_column() {
                    rule.value.clone().unwrap_or_default()
                } else {
                    rule.predicate.clone()
                }
            })
            .unwrap_or_default()
    }

    /// Point a column rule at another accepted output, keeping its value:
    /// re classification never rewrites what it classifies against.
    fn cycle_column(&mut self, delta: i32, ctx: &mut Ctx<'_>) {
        let options = Self::classifiable_columns(ctx);
        if options.is_empty() {
            return;
        }
        let selected = self.selected;
        Self::with_draft(ctx, |rules| {
            let Some(rule) = rules.get_mut(selected) else {
                return;
            };
            if !rule.is_column() {
                return;
            }
            let current = rule.column.clone().unwrap_or_default();
            let at = options
                .iter()
                .position(|name| *name == current)
                .unwrap_or(if delta >= 0 { options.len() - 1 } else { 0 });
            let next =
                options[(at as i32 + delta).rem_euclid(options.len() as i32) as usize].clone();
            rule.column = Some(next);
        });
        self.reset_cursor(ctx);
    }

    /// Whether the predicate field currently has the keys, which is what makes
    /// `q` a character rather than a dismissal (§1).
    fn editing(&self) -> bool {
        self.control == ColorRulesControl::Predicate
    }

    fn controls(&self, ctx: &Ctx<'_>) -> Vec<ColorRulesControl> {
        let rules = Self::draft(ctx).len();
        let mut controls = vec![ColorRulesControl::List];
        if rules > 0 {
            if Self::selected_is_column(ctx, self.selected) {
                controls.push(ColorRulesControl::Column);
            }
            controls.push(ColorRulesControl::Predicate);
            controls.push(ColorRulesControl::Color);
        }
        controls.push(ColorRulesControl::Add);
        if rules > 0 {
            controls.push(ColorRulesControl::Remove);
        }
        controls.push(ColorRulesControl::Apply);
        controls
    }

    fn move_control(&mut self, delta: i32, ctx: &Ctx<'_>) {
        let controls = self.controls(ctx);
        if controls.is_empty() {
            return;
        }
        let at = controls
            .iter()
            .position(|control| *control == self.control)
            .unwrap_or(0);
        // Focus may land on an action the last render hid behind `More ▾`;
        // that is not an invisible stop, because the More button owns the
        // focus ring then and Enter opens the menu (see `activate`). Hidden
        // actions stay directly reachable through their mnemonics too.
        self.control = controls[(at as i32 + delta).rem_euclid(controls.len() as i32) as usize];
        if self.editing() {
            self.reset_cursor(ctx);
        }
    }

    /// The overflow menu drives while it is active: selection wraps over the
    /// stored overflow list, activation routes through `press_action` with the
    /// ORIGINAL action index so a menu item runs exactly what its button
    /// would.
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

    fn action_index(control: ColorRulesControl) -> Option<usize> {
        match control {
            ColorRulesControl::Add => Some(0),
            ColorRulesControl::Remove => Some(1),
            ColorRulesControl::Apply => Some(2),
            _ => None,
        }
    }

    fn reset_cursor(&mut self, ctx: &Ctx<'_>) {
        reset_cursor_to_end(&Self::selected_text(ctx, self.selected), &mut self.cursor);
    }

    fn move_selection(&mut self, delta: i32, ctx: &mut Ctx<'_>) {
        let rules = Self::draft(ctx).len();
        if rules == 0 {
            return;
        }
        self.commit_empty_addition(ctx);
        let rules = Self::draft(ctx).len();
        if rules == 0 {
            self.selected = 0;
            return;
        }
        self.selected = (self.selected as i32 + delta).rem_euclid(rules as i32) as usize;
        self.reset_cursor(ctx);
    }

    /// A rule added and then abandoned without its match text is removed
    /// rather than left in the list matching nothing: the exact value for a
    /// column rule, the predicate for a legacy one.
    fn commit_empty_addition(&mut self, ctx: &mut Ctx<'_>) {
        if !self.adding {
            return;
        }
        self.adding = false;
        let selected = self.selected;
        let empty = Self::draft(ctx).get(selected).is_some_and(|rule| {
            if rule.is_column() {
                rule.value.as_deref().is_none_or(|value| value.is_empty())
            } else {
                rule.predicate.trim().is_empty()
            }
        });
        if empty {
            Self::with_draft(ctx, |rules| {
                if selected < rules.len() {
                    rules.remove(selected);
                }
            });
            self.selected = selected.saturating_sub(1);
        }
    }

    fn add(&mut self, ctx: &mut Ctx<'_>) {
        self.commit_empty_addition(ctx);
        if Self::draft(ctx).len() >= MAX_COLOR_RULES {
            Self::error(
                ctx,
                &format!("at most {MAX_COLOR_RULES} colour rules in one view"),
            );
            return;
        }
        // A new rule takes the next colour in the palette, so a list built by
        // pressing Add repeatedly is legible without choosing anything.
        let color = RuleColor::ALL[Self::draft(ctx).len() % RuleColor::ALL.len()];
        // Normal entry classifies an enrichment column: patterns and keys
        // belong in ordinary enrichment definitions, so a new rule never
        // starts life as an independent raw pattern classifier. With no
        // accepted outputs yet there is nothing to classify and the rule
        // starts as the explicit raw-text exception instead.
        let column = Self::classifiable_columns(ctx).into_iter().next();
        Self::with_draft(ctx, |rules| {
            if let Some(column) = column {
                rules.push(ColorRule::column_rule(column, String::new(), color));
            } else {
                rules.push(ColorRule::predicate_rule(String::new(), color));
            }
        });
        self.selected = Self::draft(ctx).len().saturating_sub(1);
        self.control = ColorRulesControl::Predicate;
        self.adding = true;
        self.reset_cursor(ctx);
    }

    fn remove(&mut self, ctx: &mut Ctx<'_>) {
        let selected = self.selected;
        if selected >= Self::draft(ctx).len() {
            return;
        }
        self.adding = false;
        Self::with_draft(ctx, |rules| {
            rules.remove(selected);
        });
        self.selected = selected.min(Self::draft(ctx).len().saturating_sub(1));
        if Self::draft(ctx).is_empty() {
            self.control = ColorRulesControl::List;
        }
        self.reset_cursor(ctx);
    }

    fn cycle_color(&mut self, delta: i32, ctx: &mut Ctx<'_>) {
        let selected = self.selected;
        Self::with_draft(ctx, |rules| {
            if let Some(rule) = rules.get_mut(selected) {
                let at = RuleColor::ALL
                    .iter()
                    .position(|color| *color == rule.color)
                    .unwrap_or(0);
                rule.color = RuleColor::ALL
                    [(at as i32 + delta).rem_euclid(RuleColor::ALL.len() as i32) as usize];
            }
        });
    }

    fn text(&mut self, command: EditCommand<'_>, ctx: &mut Ctx<'_>) -> Outcome {
        if !self.editing() {
            return Outcome::Ignored;
        }
        let selected = self.selected;
        if Self::draft(ctx).get(selected).is_none() {
            return Outcome::Ignored;
        }
        let column = Self::selected_is_column(ctx, selected);
        let mut value = Self::selected_text(ctx, selected);
        let mut cursor = self.cursor;
        let outcome = edit(
            &mut value,
            &mut cursor,
            command,
            EditPolicy {
                max_bytes: MAX_PREDICATE_BYTES,
                multiline: false,
            },
        );
        self.cursor = cursor;
        if outcome.changed {
            // Authored text settles the new rule: clearing it afterwards
            // reads as an (empty, valid) edit rather than an abandoned
            // addition, which removes itself instead.
            self.adding = false;
            Self::with_draft(ctx, |rules| {
                if let Some(rule) = rules.get_mut(selected) {
                    if column {
                        rule.value = Some(value);
                    } else {
                        rule.predicate = value;
                    }
                }
            });
        }
        if outcome.changed || outcome.moved {
            Outcome::Consumed
        } else {
            Outcome::Ignored
        }
    }

    /// What can be checked without data. Everything else — a field that does
    /// not exist, a Polars expression that will not compile — is the engine's
    /// answer, and it arrives as an uncoloured row rather than as a broken view.
    fn validate(rules: &[ColorRule]) -> Result<(), String> {
        for (index, rule) in rules.iter().enumerate() {
            let position = index + 1;
            if rule.is_column() {
                if rule
                    .column
                    .as_deref()
                    .is_some_and(|column| column.trim().is_empty())
                    || rule.column.is_none()
                {
                    return Err(format!(
                        "rule {position} names no column — pick an enrichment output"
                    ));
                }
                // An empty-string value is valid: it matches only literal
                // empty-string ready cells. A missing value is malformed and
                // rejected loudly at execution instead.
                continue;
            }
            if rule.predicate.trim().is_empty() {
                return Err(format!("rule {position} has no predicate"));
            }
            let body = rule.predicate.trim();
            if let Some(pattern) = body.strip_prefix('/')
                && !body.starts_with(r"\/")
            {
                let (pattern, flags) = pattern.rsplit_once('/').unwrap_or((pattern, ""));
                let source = if flags.is_empty() {
                    pattern.to_owned()
                } else {
                    format!("(?{flags}){pattern}")
                };
                if let Err(error) = regex::RegexBuilder::new(&source)
                    .size_limit(1024 * 1024)
                    .nest_limit(64)
                    .build()
                {
                    return Err(format!("rule {position}: invalid regex · {error}"));
                }
            }
        }
        Ok(())
    }

    fn apply(&mut self, ctx: &mut Ctx<'_>) -> Outcome {
        self.commit_empty_addition(ctx);
        let Some(view_id) = ctx.views.active_id().map(str::to_owned) else {
            return Outcome::Consumed;
        };
        let draft = Self::draft(ctx);
        if let Err(message) = Self::validate(&draft) {
            Self::error(ctx, &message);
            return Outcome::Consumed;
        }
        // Colour rules are display-only, so they ride the same query the
        // display-only grouping rule does; the applied view stays on screen
        // until the answer arrives.
        match ctx.views.enqueue_presentation(&view_id, draft) {
            Ok(_) => {
                ctx.views.touch(&view_id);
                Outcome::Consumed
            }
            Err(SubmitRefused::QueueFull) => {
                Self::error(ctx, "query queue is full · the rules were kept");
                Outcome::Consumed
            }
        }
    }

    fn activate(&mut self, ctx: &mut Ctx<'_>) -> Outcome {
        // Enter on an action the last render hid behind `More ▾` opens the
        // menu on that action instead of running it blind: the More button
        // owns the focus ring then. Painted actions run directly below.
        // (An open menu never reaches here: Enter drives it in `key`.)
        if let Some(index) = Self::action_index(self.control) {
            let painted = self
                .geometry
                .controls
                .iter()
                .any(|(_, control)| *control == self.control);
            if !painted && self.geometry.more_overflow.contains(&index) {
                self.open_more(Some(index));
                return Outcome::Consumed;
            }
        }
        match self.control {
            // Enter on the column chooser applies, like Enter on the colour
            // chooser: choosing is done with Left/Right.
            ColorRulesControl::Column => self.apply(ctx),
            ColorRulesControl::Add => {
                self.add(ctx);
                Outcome::Consumed
            }
            ColorRulesControl::Remove => {
                self.remove(ctx);
                Outcome::Consumed
            }
            ColorRulesControl::Color => {
                self.cycle_color(1, ctx);
                Outcome::Consumed
            }
            ColorRulesControl::List => {
                // Enter on the list edits the rule under the cursor, which is
                // the only thing there is to do with it. On an empty list there
                // is no row to edit, so it runs the default, which is `Add`
                // (§8.9, the same shape as Enrichment's empty chain).
                if Self::draft(ctx).is_empty() {
                    self.add(ctx);
                } else {
                    self.control = ColorRulesControl::Predicate;
                    self.reset_cursor(ctx);
                }
                Outcome::Consumed
            }
            ColorRulesControl::Predicate | ColorRulesControl::Apply => self.apply(ctx),
        }
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
        // The overflow menu is transient: while it is active its own arrows
        // and Enter drive it, and any other key dismisses it first and then
        // processes normally, so typing never lands behind an open menu. A
        // stale open without overflow (cleared geometry) just drops.
        if self.more_active() {
            match key.code {
                KeyCode::Up => {
                    self.move_more(-1);
                    return Outcome::Consumed;
                }
                KeyCode::Down => {
                    self.move_more(1);
                    return Outcome::Consumed;
                }
                KeyCode::Enter => {
                    return self.activate_more(ctx);
                }
                _ => {
                    self.more_open = false;
                }
            }
        } else if self.more_open {
            self.more_open = false;
        }
        match key.code {
            KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.move_control(-1, ctx);
                Outcome::Consumed
            }
            KeyCode::BackTab => {
                self.move_control(-1, ctx);
                Outcome::Consumed
            }
            KeyCode::Tab => {
                self.move_control(1, ctx);
                Outcome::Consumed
            }
            // Left/Right choose the colour while the chooser has focus, the
            // column while its chooser does, and move the caret while the
            // field does.
            KeyCode::Left if self.control == ColorRulesControl::Color => {
                self.cycle_color(-1, ctx);
                Outcome::Consumed
            }
            KeyCode::Right if self.control == ColorRulesControl::Color => {
                self.cycle_color(1, ctx);
                Outcome::Consumed
            }
            KeyCode::Left if self.control == ColorRulesControl::Column => {
                self.cycle_column(-1, ctx);
                Outcome::Consumed
            }
            KeyCode::Right if self.control == ColorRulesControl::Column => {
                self.cycle_column(1, ctx);
                Outcome::Consumed
            }
            KeyCode::Left if self.editing() => self.text(EditCommand::MoveLeft, ctx),
            KeyCode::Right if self.editing() => self.text(EditCommand::MoveRight, ctx),
            KeyCode::Up => {
                self.move_selection(-1, ctx);
                Outcome::Consumed
            }
            KeyCode::Down => {
                self.move_selection(1, ctx);
                Outcome::Consumed
            }
            KeyCode::Enter => self.activate(ctx),
            KeyCode::Backspace => self.text(EditCommand::Backspace, ctx),
            KeyCode::Char(character) => {
                let mut buffer = [0u8; 4];
                self.text(EditCommand::Insert(character.encode_utf8(&mut buffer)), ctx)
            }
            _ => Outcome::Ignored,
        }
    }

    fn mouse(
        &mut self,
        kind: MouseEventKind,
        hit: Option<ColorRulesHit>,
        ctx: &mut Ctx<'_>,
    ) -> Outcome {
        let pressed = matches!(kind, MouseEventKind::Down(MouseButton::Left));
        // A drawn overflow menu owns its clicks: rows activate through
        // `press_action`, its button toggles it shut, and anything else
        // dismisses it — the dialog underneath is not clickable through it.
        // (`hit` only yields menu rows from a painted menu.)
        if !self.geometry.more_rows.is_empty() {
            match (pressed, hit) {
                (true, Some(ColorRulesHit::More(Some(index)))) => {
                    self.more_open = false;
                    return self.press_action(index, ctx);
                }
                (true, Some(ColorRulesHit::More(None))) => {
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
        if pressed && matches!(hit, Some(ColorRulesHit::More(None))) {
            self.open_more(None);
            return Outcome::Consumed;
        }
        match (pressed, hit) {
            (true, Some(ColorRulesHit::Swatch(index))) => {
                self.selected = index;
                self.control = ColorRulesControl::Color;
                self.cycle_color(1, ctx);
                return Outcome::Consumed;
            }
            (true, Some(ColorRulesHit::Row(index))) => {
                self.commit_empty_addition(ctx);
                self.selected = index.min(Self::draft(ctx).len().saturating_sub(1));
                self.control = ColorRulesControl::List;
                self.reset_cursor(ctx);
                return Outcome::Consumed;
            }
            (true, Some(ColorRulesHit::Control(control))) => {
                if self.controls(ctx).contains(&control) {
                    self.control = control;
                    if self.editing() {
                        self.reset_cursor(ctx);
                    }
                }
                if !matches!(
                    control,
                    ColorRulesControl::List
                        | ColorRulesControl::Column
                        | ColorRulesControl::Predicate
                ) {
                    return self.activate(ctx);
                }
                return Outcome::Consumed;
            }
            _ => {}
        }
        match kind {
            MouseEventKind::ScrollUp => self.move_selection(-1, ctx),
            MouseEventKind::ScrollDown => self.move_selection(1, ctx),
            _ => return Outcome::Ignored,
        }
        Outcome::Consumed
    }
}

fn contains(area: Rect, point: (u16, u16)) -> bool {
    point.0 >= area.x && point.0 < area.right() && point.1 >= area.y && point.1 < area.bottom()
}

/// §4.3: the palette entry this layer owns.
const APPLY_COMMAND: CommandSpec = CommandSpec {
    id: CommandId::ColorRulesApply,
    name: "Apply colour rules",
    description: "Repaint the view with the edited rules",
    category: "View",
    aliases: &["recolour", "highlight rules"],
    shortcut: None,
};

impl Component for ColorRulesDialog {
    type Hit = ColorRulesHit;
    type Open = ();

    fn open(&mut self, _params: (), ctx: &mut Ctx<'_>) {
        self.open = true;
        self.selected = 0;
        self.top = 0;
        self.adding = false;
        self.control = ColorRulesControl::List;
        self.more_open = false;
        self.more_selected = 0;
        self.more_first = 0;
        self.geometry = ColorRulesGeometry::default();
        // The dialog resumes an unfinished edit, and seeds from the accepted
        // rules when there is none: an empty draft beside accepted rules would
        // read as "no rules" and apply as "delete them all".
        if let Some(view_id) = ctx.views.active_id().map(str::to_owned)
            && let Some(state) = ctx.views.state_mut(&view_id)
            && state.color_rules_draft.is_empty()
        {
            state.color_rules_draft = state.color_rules.clone();
        }
        self.reset_cursor(ctx);
    }

    fn handle(&mut self, event: Event<ColorRulesHit>, ctx: &mut Ctx<'_>) -> Outcome {
        match event {
            Event::Key(key) => self.key(key, ctx),
            Event::Paste(text) => self.text(EditCommand::Insert(&text), ctx),
            Event::Mouse { kind, hit, .. } => self.mouse(kind, hit, ctx),
            Event::Dismiss => {
                // §10 frontmost-first: the overflow menu closes before the
                // dialog it hangs from.
                if self.more_open {
                    self.more_open = false;
                    return Outcome::Consumed;
                }
                self.commit_empty_addition(ctx);
                self.open = false;
                Outcome::Close
            }
            Event::Command(CommandId::ColorRulesApply) => {
                // Palette commands act on the dialog, dismissing the transient menu.
                self.more_open = false;
                self.apply(ctx)
            }
            Event::Command(_) | Event::View(ViewEvent::SourcesChanged { .. }) => Outcome::Ignored,
            Event::View(_) | Event::Resize => Outcome::Ignored,
        }
    }

    fn commands(&self, _views: &Views) -> Vec<CommandEntry> {
        vec![CommandEntry {
            spec: CommandSpec {
                shortcut: self.open.then_some("Enter"),
                ..APPLY_COMMAND
            },
            unavailable_reason: (!self.open).then_some("open Colour rules first"),
        }]
    }

    fn action_labels(&self, _ctx: &Ctx<'_>) -> Vec<&'static str> {
        COLOR_RULES_BUTTONS.to_vec()
    }

    fn press_action(&mut self, index: usize, ctx: &mut Ctx<'_>) -> Outcome {
        // Any press dismisses the transient overflow menu first — including
        // the menu's own items, which arrive here with original indices (0
        // Add, 1 Remove, 2 Apply) and run exactly what their buttons would.
        self.more_open = false;
        match index {
            0 => self.add(ctx),
            1 => self.remove(ctx),
            2 => return self.apply(ctx),
            _ => return Outcome::Ignored,
        }
        Outcome::Consumed
    }

    fn surface(&self) -> Surface {
        Surface {
            text_focus: self.editing(),
            ..self.surface
        }
    }

    fn hit(&self, point: (u16, u16)) -> Option<ColorRulesHit> {
        let g = &self.geometry;
        // A drawn overflow menu sits above the dialog: its rows first, then
        // its button, mirroring paint order. The closed menu's button is
        // hit-tested with the dialog controls below.
        if !g.more_rows.is_empty() {
            return g
                .more_rows
                .iter()
                .find_map(|(rect, index)| {
                    contains(*rect, point).then_some(ColorRulesHit::More(Some(*index)))
                })
                .or_else(|| {
                    g.more_button
                        .filter(|rect| contains(*rect, point))
                        .map(|_| ColorRulesHit::More(None))
                })
                .or(Some(ColorRulesHit::Body));
        }
        if let Some(rect) = g.more_button
            && contains(rect, point)
        {
            return Some(ColorRulesHit::More(None));
        }
        g.swatches
            .iter()
            .find_map(|(rect, index)| {
                contains(*rect, point).then_some(ColorRulesHit::Swatch(*index))
            })
            .or_else(|| {
                g.controls.iter().find_map(|(rect, control)| {
                    contains(*rect, point).then_some(ColorRulesHit::Control(*control))
                })
            })
            .or_else(|| {
                g.rows.iter().find_map(|(rect, index)| {
                    contains(*rect, point).then_some(ColorRulesHit::Row(*index))
                })
            })
            .or_else(|| contains(g.body, point).then_some(ColorRulesHit::Body))
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx<'_>) -> Surface {
        let theme = ctx.theme;
        let ascii = ctx.ascii;
        let styles = DialogStyles::new(theme);
        let (rules, accepted, error) = ctx.views.active().map_or_else(
            || (Vec::new(), Vec::new(), None),
            |state| {
                (
                    state.color_rules_draft.clone(),
                    state.color_rules.clone(),
                    state.color_rules_error.clone(),
                )
            },
        );
        let mut rows: Vec<(Rect, usize)> = Vec::new();
        let mut swatches: Vec<(Rect, usize)> = Vec::new();
        let mut caret: Option<(u16, u16)> = None;

        let dirty = rules != accepted;
        let (state, sentence) = match error.as_deref() {
            Some(error) => (MessageState::Error, error.to_owned()),
            None if dirty => (
                MessageState::Pending,
                "edited · Apply to repaint the view".to_owned(),
            ),
            None if accepted.is_empty() => (
                MessageState::Disabled,
                "no rules · rows keep their field or severity colour".to_owned(),
            ),
            None => (
                MessageState::Applied,
                format!(
                    "{} rule{} painting this view",
                    accepted.len(),
                    if accepted.len() == 1 { "" } else { "s" }
                ),
            ),
        };
        let help = "The first matching rule wins. A rule classifies an enrichment column's exact value; raw text is the explicit exception, and earlier predicates keep working.";
        let labels = COLOR_RULES_BUTTONS;
        let has_column_editor = rules
            .get(self.selected)
            .is_some_and(|rule| rule.is_column());
        // Natural body rows for the scroll extent alone (never the frame):
        // the list heading, one row per rule (or the single empty-state
        // line), then — when rules exist — a gap row plus the editor rows
        // (Column/Value/Colour for a column rule, Predicate/Colour otherwise).
        let rule_rows = rules.len().max(1);
        let editor_rows = if rules.is_empty() {
            0
        } else if has_column_editor {
            3
        } else {
            2
        };
        let content_rows = 1usize
            .saturating_add(rule_rows)
            .saturating_add(if rules.is_empty() { 0 } else { 1 + editor_rows });
        // First editor row in logical coordinates (the gap row sits before it).
        let editor_base = 1usize.saturating_add(rule_rows).saturating_add(1);
        let spec = color_spec(area);
        let Ok(geometry) = crate::dialog_layout::resolve_dialog(
            area,
            &spec,
            content_rows,
            &labels,
            Some(if rules.is_empty() { 0 } else { 2 }),
            ctx.context_anchor,
        ) else {
            // Below the 20x6 floor the tiny fallback owns the frame; stay open
            // with nothing drawn, as the palette does.
            self.geometry = ColorRulesGeometry::default();
            self.surface = Surface::default();
            return self.surface;
        };
        render_responsive_frame(frame, &geometry, "Colour rules", true, theme);
        // One authoritative geometry for frame/anatomy/body/actions. The same
        // projected rects drive paint, caret, scrollbar, selection and mouse;
        // no independent outer calculation.
        let mut surface = Surface {
            popup: geometry.frame,
            interior: geometry.interior,
            caret: None,
            scrollable: geometry.body.overflow() > 0,
            text_focus: self.editing(),
        };
        self.geometry.body = geometry.body.viewport;

        // Shared body projection with focus reveal: the editor row being
        // edited while an editor control has focus, otherwise the selected
        // rule — the list always keeps its selection visible, as before, and
        // the sticky action band needs no reveal.
        let mut body = ScrollViewport::new(geometry.body.viewport, content_rows, 0);
        if !rules.is_empty() {
            let selected_row = 1usize.saturating_add(self.selected.min(rules.len() - 1));
            let reveal_row = match self.control {
                ColorRulesControl::Column => editor_base,
                ColorRulesControl::Predicate => {
                    editor_base.saturating_add(usize::from(has_column_editor))
                }
                ColorRulesControl::Color => {
                    editor_base.saturating_add(editor_rows.saturating_sub(1))
                }
                ColorRulesControl::List
                | ColorRulesControl::Add
                | ColorRulesControl::Remove
                | ColorRulesControl::Apply => selected_row,
            };
            body = ScrollViewport::new(
                geometry.body.viewport,
                content_rows,
                body.reveal(reveal_row),
            );
        }
        self.top = body.first_row.saturating_sub(1).min(rules.len());
        if let Some(bar) = body.scrollbar {
            render_scrollbar(frame, bar, body.first_row, body.overflow(), theme, ascii);
        }

        let mut controls: Vec<(Rect, ColorRulesControl)> = Vec::new();
        // §8.7 list heading with right-aligned count, painted in the shared
        // viewport like every other row.
        if let Some(heading) = body.project_row(0) {
            let count = format!(
                "{} of {}",
                self.selected.saturating_add(1).min(rules.len().max(1)),
                rules.len()
            );
            let count_width = UnicodeWidthStr::width(count.as_str());
            let filler = usize::from(heading.width)
                .saturating_sub(UnicodeWidthStr::width("Rules"))
                .saturating_sub(count_width);
            frame.render_widget(
                Paragraph::new(ratatui::text::Line::from(vec![
                    ratatui::text::Span::styled("Rules", styles.label.add_modifier(Modifier::BOLD)),
                    ratatui::text::Span::styled(" ".repeat(filler), styles.description),
                    ratatui::text::Span::styled(count, styles.description),
                ])),
                heading,
            );
            // Only the heading is a control; the rows underneath are rows.
            controls.push((heading, ColorRulesControl::List));
        }

        if rules.is_empty()
            && let Some(empty) = body.project_row(1)
        {
            frame.render_widget(
                Paragraph::new("no rules yet · Add one").style(styles.unavailable),
                empty,
            );
        }
        for (index, rule) in rules.iter().enumerate() {
            let Some(row) = body.project_row(1 + index) else {
                continue;
            };
            let chosen = index == self.selected;
            let marker = if chosen {
                if ascii { "> " } else { "› " }
            } else {
                "  "
            };
            // The swatch is drawn in the colour it names, so the list answers
            // "what will this look like" without applying anything.
            let swatch = if ascii { "##" } else { "██" };
            frame.render_widget(
                Paragraph::new(format!("{marker}{}", index + 1)).style(if chosen {
                    styles.selection
                } else {
                    styles.description
                }),
                row,
            );
            let swatch_x = row.x.saturating_add(4);
            if swatch_x < row.right() {
                let swatch_rect = Rect::new(swatch_x, row.y, 2.min(row.right() - swatch_x), 1);
                frame.render_widget(
                    Paragraph::new(swatch)
                        .style(styles.description.fg(theme.rule_color(rule.color))),
                    swatch_rect,
                );
                swatches.push((swatch_rect, index));
            }
            let text_x = row.x.saturating_add(7);
            if text_x < row.right() {
                // A column rule reads as what it classifies; a legacy rule
                // as the predicate it still evaluates.
                let summary = rule.summary();
                frame.render_widget(
                    Paragraph::new(clipped_width(&summary, usize::from(row.right() - text_x)))
                        .style(if chosen {
                            styles.selection
                        } else {
                            styles.description
                        }),
                    Rect::new(text_x, row.y, row.right() - text_x, 1),
                );
            }
            rows.push((row, index));
        }

        // The editor for the selected rule: column chooser plus value field
        // plus colour chooser for a column rule; predicate field plus colour
        // chooser for a legacy one. Every row is projected through the same
        // shared viewport, so caret, selection and mouse agree; rows scrolled
        // out paint nothing and claim no hitbox.
        let label_width = u16::try_from(UnicodeWidthStr::width("Predicate")).unwrap_or(9);
        // The column chooser's hitbox, registered with the action controls
        // below when a column rule is selected.
        let mut column_rect: Option<Rect> = None;
        if !rules.is_empty() {
            let field_x = body
                .viewport
                .x
                .saturating_add(label_width)
                .saturating_add(FIELD_GUTTER);
            let mut editor_row = editor_base;
            if has_column_editor {
                if let Some(label) = body.project_row(editor_row) {
                    let column = rules
                        .get(self.selected)
                        .and_then(|rule| rule.column.clone())
                        .unwrap_or_default();
                    frame.render_widget(
                        Paragraph::new("Column").style(
                            if self.control == ColorRulesControl::Column {
                                styles.shortcut.add_modifier(Modifier::BOLD)
                            } else {
                                styles.label
                            },
                        ),
                        Rect::new(label.x, label.y, label_width.min(label.width), 1),
                    );
                    if field_x < label.right() {
                        let chooser = Rect::new(field_x, label.y, label.right() - field_x, 1);
                        frame.render_widget(
                            Paragraph::new(truncated(
                                &format!("‹ {column} ›"),
                                usize::from(label.right() - field_x),
                            ))
                            .style(styles.description),
                            chooser,
                        );
                        column_rect = Some(chooser);
                    }
                }
                editor_row = editor_row.saturating_add(1);
            }
            let field_label = if has_column_editor {
                "Value"
            } else {
                "Predicate"
            };
            if let Some(label) = body.project_row(editor_row) {
                frame.render_widget(
                    Paragraph::new(field_label).style(if self.editing() {
                        styles.shortcut.add_modifier(Modifier::BOLD)
                    } else {
                        styles.label
                    }),
                    Rect::new(label.x, label.y, label_width.min(label.width), 1),
                );
                if field_x < label.right() {
                    let field = Rect::new(field_x, label.y, label.right() - field_x, 1);
                    let predicate = rules
                        .get(self.selected)
                        .map(|rule| {
                            if rule.is_column() {
                                rule.value.clone().unwrap_or_default()
                            } else {
                                rule.predicate.clone()
                            }
                        })
                        .unwrap_or_default();
                    if self.editing() {
                        caret = place_input_cursor_at(
                            frame,
                            field,
                            0,
                            0,
                            &predicate,
                            self.cursor.char_index,
                            theme,
                        );
                    } else {
                        frame.render_widget(
                            Paragraph::new(truncated(&predicate, usize::from(field.width)))
                                .style(styles.description),
                            field,
                        );
                    }
                }
            }
            editor_row = editor_row.saturating_add(1);
            if let Some(label) = body.project_row(editor_row) {
                frame.render_widget(
                    Paragraph::new("Colour").style(if self.control == ColorRulesControl::Color {
                        styles.shortcut.add_modifier(Modifier::BOLD)
                    } else {
                        styles.label
                    }),
                    Rect::new(label.x, label.y, label_width.min(label.width), 1),
                );
                if field_x < label.right() {
                    let color = rules
                        .get(self.selected)
                        .map(|rule| rule.color)
                        .unwrap_or_default();
                    frame.render_widget(
                        Paragraph::new(truncated(
                            &format!("‹ {} ›", color.label()),
                            usize::from(label.right() - field_x),
                        ))
                        .style(styles.description.fg(theme.rule_color(color))),
                        Rect::new(field_x, label.y, label.right() - field_x, 1),
                    );
                }
            }
        }

        render_message(frame, geometry.message, state, &sentence, theme, ascii);
        render_help_text(frame, geometry.help, help, theme);
        // §8.9: the verb is the default, and a destructive action never is.
        // With nothing to apply yet the fill moves to `Add`, the only button
        // that does anything on an empty list. Roles are presentation only;
        // the rects come from the one shared action geometry, so click and
        // paint cannot disagree.
        let default_idx = if rules.is_empty() { 0 } else { 2 };
        let focused_idx = match self.control {
            ColorRulesControl::Add => Some(0),
            ColorRulesControl::Remove => Some(1),
            ColorRulesControl::Apply => Some(2),
            _ => None,
        };
        // The live overflow list and More button rect flow into the stored
        // geometry below with the rest of this frame's rects; event paths
        // between renders read that recorded copy.
        let live_overflow = geometry.actions.overflow.clone();
        let live_more_button = geometry.actions.more;
        // A focused action hidden behind `More ▾` maps its focus ring onto the
        // More button: Tab never lands invisibly.
        let hidden_focused = focused_idx.is_some_and(|index| live_overflow.contains(&index));
        for (index, rect) in &geometry.actions.buttons {
            let control = match index {
                0 => ColorRulesControl::Add,
                1 => ColorRulesControl::Remove,
                _ => ColorRulesControl::Apply,
            };
            if let Some(label) = labels.get(*index) {
                let role = if *index == 1 {
                    ButtonRole::Destructive
                } else if *index == default_idx {
                    ButtonRole::Default
                } else {
                    ButtonRole::Normal
                };
                render_role_button(
                    frame,
                    *rect,
                    label,
                    role,
                    focused_idx == Some(*index),
                    theme,
                );
            }
            controls.push((*rect, control));
        }
        if let Some(more) = live_more_button {
            render_role_button(
                frame,
                more,
                crate::dialog_controls::MORE_LABEL,
                ButtonRole::Normal,
                self.more_open || hidden_focused,
                theme,
            );
        }
        // Action overflow (`More ▾`) menu, painted last so paint order matches
        // hit-test order (menu rows first). Roles parallel `labels` with the
        // filled non-destructive default retained; activation routes through
        // `press_action` with original indices.
        let mut more_rows = Vec::new();
        let mut menu_overflows = false;
        if self.more_open && !live_overflow.is_empty() {
            if let Some(anchor) = live_more_button {
                let len = live_overflow.len();
                let sel = self.more_selected.min(len - 1);
                let roles = [
                    if rules.is_empty() {
                        ButtonRole::Default
                    } else {
                        ButtonRole::Normal
                    },
                    ButtonRole::Destructive,
                    if rules.is_empty() {
                        ButtonRole::Normal
                    } else {
                        ButtonRole::Default
                    },
                ];
                let (popup, rows, first, overflows) = draw_action_menu(
                    frame,
                    area,
                    anchor,
                    &labels,
                    &roles,
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
                    menu_overflows = overflows;
                    more_rows = rows;
                } else {
                    // Refused placement (no gap-honoring bands): the menu
                    // cannot show, so it is not open.
                    self.more_open = false;
                }
            } else {
                self.more_open = false;
            }
        } else {
            // Overflow gone (regrew wider) means the menu has nothing to show.
            self.more_open = false;
        }
        // The column chooser answers clicks exactly like the colour chooser.
        if let Some(rect) = column_rect {
            controls.push((rect, ColorRulesControl::Column));
        }
        surface.caret = caret;
        surface.scrollable = body.overflow() > 0 || menu_overflows;
        self.geometry = ColorRulesGeometry {
            body: geometry.body.viewport,
            rows,
            swatches,
            controls,
            more_button: live_more_button,
            more_overflow: live_overflow,
            more_rows,
            action_band: geometry.actions.band,
        };
        self.surface = surface;
        surface
    }
}
