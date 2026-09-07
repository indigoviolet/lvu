//! The Colour rules layer (`c`): an ordered, per-view list of
//! "when <predicate> then <colour>" rules that decide how a row is painted.
//!
//! Everything it edits is view-owned (§2.5). `ViewState.color_rules` is the
//! accepted list — what the rows on screen were painted with — and
//! `color_rules_draft` is the working list the dialog edits, kept per view so
//! closing and reopening resumes the edit and a restart restores it. Nothing is
//! cached here; the layer renders from `ctx.views.active()` every frame.
//!
//! Two things this layer deliberately does not do:
//!
//! * **It does not evaluate a predicate.** A rule is written in the search
//!   box's own language and is compiled and run by the query engine, in the
//!   same batch pass and through the same `TextSearch` the filter uses. What
//!   this dialog validates is only what can be checked without data — that the
//!   predicate is non-empty, within the byte cap, and, for a `/regex/`, that it
//!   compiles. Everything else is the engine's answer.
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
use crate::dialog_controls::{ActionRow, DialogStyles};
use crate::text_edit::{EditCommand, EditPolicy, TextCursor, edit, reset_cursor_to_end};
use crate::ui::{
    FIELD_GUTTER, MessageState, clipped_width, dialog_frame_regions, help_rows, message_rows,
    packed_button_rows, place_input_cursor_at, render_actions, render_help_text, render_message,
    render_pane_heading, render_scrollbar, truncated,
};

/// A predicate is a filter expression, not a document.
const MAX_PREDICATE_BYTES: usize = 512;

/// Which control has focus. UI-only state, so it lives here (§3).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ColorRulesControl {
    /// The rule list.
    #[default]
    List,
    /// The predicate field for the rule being added or edited.
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
    Control(ColorRulesControl),
    Body,
}

#[derive(Clone, Debug, Default)]
struct ColorRulesGeometry {
    body: Rect,
    rows: Vec<(Rect, usize)>,
    swatches: Vec<(Rect, usize)>,
    controls: Vec<(Rect, ColorRulesControl)>,
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
    geometry: ColorRulesGeometry,
    surface: Surface,
}

/// The action row, in drawn order, with its §8.10 mnemonics. `Add` takes the
/// `A`, so `Apply` underlines its `p`; the shell resolves both from these
/// labels, which is why the dialog keeps no Alt keymap of its own.
const COLOR_RULES_BUTTONS: [&str; 3] = ["&Add", "&Remove", "A&pply"];

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

    /// Whether the predicate field currently has the keys, which is what makes
    /// `q` a character rather than a dismissal (§1).
    fn editing(&self) -> bool {
        self.control == ColorRulesControl::Predicate
    }

    fn controls(&self, rules: usize) -> Vec<ColorRulesControl> {
        let mut controls = vec![ColorRulesControl::List];
        if rules > 0 {
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
        let controls = self.controls(Self::draft(ctx).len());
        let at = controls
            .iter()
            .position(|control| *control == self.control)
            .unwrap_or(0);
        self.control = controls[(at as i32 + delta).rem_euclid(controls.len() as i32) as usize];
        if self.editing() {
            self.reset_cursor(ctx);
        }
    }

    fn reset_cursor(&mut self, ctx: &Ctx<'_>) {
        let value = Self::draft(ctx)
            .get(self.selected)
            .map(|rule| rule.predicate.clone())
            .unwrap_or_default();
        reset_cursor_to_end(&value, &mut self.cursor);
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

    /// A rule added and then abandoned without a predicate is removed rather
    /// than left in the list matching nothing.
    fn commit_empty_addition(&mut self, ctx: &mut Ctx<'_>) {
        if !self.adding {
            return;
        }
        self.adding = false;
        let selected = self.selected;
        let empty = Self::draft(ctx)
            .get(selected)
            .is_some_and(|rule| rule.predicate.trim().is_empty());
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
        Self::with_draft(ctx, |rules| {
            rules.push(ColorRule {
                predicate: String::new(),
                color,
            });
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
        let Some(mut value) = Self::draft(ctx)
            .get(selected)
            .map(|rule| rule.predicate.clone())
        else {
            return Outcome::Ignored;
        };
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
            Self::with_draft(ctx, |rules| {
                if let Some(rule) = rules.get_mut(selected) {
                    rule.predicate = value;
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
        match self.control {
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
            // Left/Right choose the colour while the chooser has focus, and
            // move the caret while the field does.
            KeyCode::Left if self.control == ColorRulesControl::Color => {
                self.cycle_color(-1, ctx);
                Outcome::Consumed
            }
            KeyCode::Right if self.control == ColorRulesControl::Color => {
                self.cycle_color(1, ctx);
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
                if self.controls(Self::draft(ctx).len()).contains(&control) {
                    self.control = control;
                    if self.editing() {
                        self.reset_cursor(ctx);
                    }
                }
                if !matches!(
                    control,
                    ColorRulesControl::List | ColorRulesControl::Predicate
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
                self.commit_empty_addition(ctx);
                self.open = false;
                Outcome::Close
            }
            Event::Command(CommandId::ColorRulesApply) => self.apply(ctx),
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
        use crate::dialog_layout::{DialogClass, DialogContent, content_width};

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

        let width = content_width(area, DialogClass::M);
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
        let help = "The first matching rule wins. A predicate is a search: text, field: value, /regex/, or a pl. expression.";
        let labels = COLOR_RULES_BUTTONS;
        let visible_rules = rules.len().clamp(1, 8);
        let content = DialogContent {
            header: 0,
            // rule pane heading + rows, a blank, then the predicate and colour rows
            body: 1 + u16::try_from(visible_rules).unwrap_or(1) + 3,
            message: message_rows(&sentence, width),
            help: help_rows(help, width),
            actions: packed_button_rows(width, &labels),
        };
        let regions =
            dialog_frame_regions(frame, area, DialogClass::M, "Colour rules", &content, theme);
        let mut surface = Surface {
            popup: regions.popup,
            interior: regions.interior,
            caret: None,
            scrollable: true,
            text_focus: self.editing(),
        };
        self.geometry = ColorRulesGeometry {
            body: regions.body,
            ..ColorRulesGeometry::default()
        };
        self.surface = surface;
        if regions.body.width == 0 || regions.body.height == 0 {
            return surface;
        }

        let list_height = u16::try_from(visible_rules)
            .unwrap_or(1)
            .saturating_add(1)
            .min(regions.body.height);
        let list_area = Rect::new(
            regions.body.x,
            regions.body.y,
            regions.body.width,
            list_height,
        );
        let count = format!(
            "{} of {}",
            self.selected.saturating_add(1).min(rules.len().max(1)),
            rules.len()
        );
        let rects = render_pane_heading(
            frame,
            list_area,
            "Rules",
            Some(count),
            rules.len().max(1),
            theme,
        );
        let visible = usize::from(rects.viewport.height);
        let first = self
            .selected
            .saturating_sub(visible.saturating_sub(1))
            .min(rules.len().saturating_sub(visible.min(rules.len())));
        self.top = first;
        if rules.is_empty() && rects.viewport.height > 0 {
            frame.render_widget(
                Paragraph::new("no rules yet · Add one").style(styles.unavailable),
                Rect::new(rects.viewport.x, rects.viewport.y, rects.viewport.width, 1),
            );
        }
        for (offset, (index, rule)) in rules
            .iter()
            .enumerate()
            .skip(first)
            .take(visible)
            .enumerate()
        {
            let row = Rect::new(
                rects.viewport.x,
                rects.viewport.y.saturating_add(offset as u16),
                rects.viewport.width,
                1,
            );
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
                frame.render_widget(
                    Paragraph::new(clipped_width(
                        &rule.predicate,
                        usize::from(row.right() - text_x),
                    ))
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
        if let Some(bar) = rects.scrollbar {
            render_scrollbar(
                frame,
                bar,
                first,
                rules.len().saturating_sub(visible),
                theme,
                ascii,
            );
        }

        // The editor for the selected rule: one labelled field and one chooser.
        let editor_y = list_area.bottom().saturating_add(1);
        let label_width = u16::try_from(UnicodeWidthStr::width("Predicate")).unwrap_or(9);
        if editor_y < regions.body.bottom() && !rules.is_empty() {
            let field_x = regions
                .body
                .x
                .saturating_add(label_width)
                .saturating_add(FIELD_GUTTER);
            frame.render_widget(
                Paragraph::new("Predicate").style(if self.editing() {
                    styles.shortcut.add_modifier(Modifier::BOLD)
                } else {
                    styles.label
                }),
                Rect::new(regions.body.x, editor_y, label_width, 1),
            );
            if field_x < regions.body.right() {
                let field = Rect::new(field_x, editor_y, regions.body.right() - field_x, 1);
                let predicate = rules
                    .get(self.selected)
                    .map(|rule| rule.predicate.clone())
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
            let color_y = editor_y.saturating_add(1);
            if color_y < regions.body.bottom() {
                frame.render_widget(
                    Paragraph::new("Colour").style(if self.control == ColorRulesControl::Color {
                        styles.shortcut.add_modifier(Modifier::BOLD)
                    } else {
                        styles.label
                    }),
                    Rect::new(regions.body.x, color_y, label_width, 1),
                );
                if field_x < regions.body.right() {
                    let color = rules
                        .get(self.selected)
                        .map(|rule| rule.color)
                        .unwrap_or_default();
                    frame.render_widget(
                        Paragraph::new(truncated(
                            &format!("‹ {} ›", color.label()),
                            usize::from(regions.body.right() - field_x),
                        ))
                        .style(styles.description.fg(theme.rule_color(color))),
                        Rect::new(field_x, color_y, regions.body.right() - field_x, 1),
                    );
                }
            }
        }

        render_message(frame, regions.message, state, &sentence, theme, ascii);
        render_help_text(frame, regions.help, help, theme);
        let focused = match self.control {
            ColorRulesControl::Add => Some(0),
            ColorRulesControl::Remove => Some(1),
            ColorRulesControl::Apply => Some(2),
            _ => None,
        };
        let mut controls: Vec<(Rect, ColorRulesControl)> =
            // §8.9: the verb is the default, and a destructive action never
            // is. With nothing to apply yet the fill moves to `Add`, the only
            // button that does anything on an empty list.
            render_actions(
                frame,
                regions.actions,
                ActionRow {
                    labels: &labels,
                    default: Some(if rules.is_empty() { 0 } else { 2 }),
                    destructive: &[1],
                    focused,
                },
                theme,
            )
                .into_iter()
                .map(|(index, rect)| {
                    (
                        rect,
                        match index {
                            0 => ColorRulesControl::Add,
                            1 => ColorRulesControl::Remove,
                            _ => ColorRulesControl::Apply,
                        },
                    )
                })
                .collect();
        // Only the heading is a control; the rows underneath are rows.
        controls.push((
            Rect::new(list_area.x, list_area.y, list_area.width, 1),
            ColorRulesControl::List,
        ));
        surface.caret = caret;
        self.geometry = ColorRulesGeometry {
            body: regions.body,
            rows,
            swatches,
            controls,
        };
        self.surface = surface;
        surface
    }
}
