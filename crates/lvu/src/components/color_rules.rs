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
    /// Compatibility-only name retained for callers compiled against the
    /// former fused editor; the manager never focuses parameter controls.
    Column,
    /// Compatibility-only former predicate/value field.
    Predicate,
    /// Compatibility-only former colour chooser.
    Color,
    Add,
    Edit,
    Remove,
    Apply,
}

/// Everything the dialog draws that can be clicked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ColorRulesHit {
    Row(usize),
    Swatch(usize),
    /// The `More ▾` button (`None`) or a row of its open overflow menu, by
    /// ORIGINAL action index (0 Add, 1 Edit, 2 Remove, 3 Apply) for `press_action`.
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
const COLOR_RULES_BUTTONS: [&str; 4] = ["&Add", "&Edit", "&Remove", "A&pply"];

/// Stable responsive budgets for the Colour rules manager.
///
/// `Contextual::Inspector` against the frozen opening-row anchor: the frame
/// avoids the referent row when possible (above/below with a one-row gap,
/// log-centered, shrunk into the larger band, top-biased fallback) and never
/// chases live selection. Outer size comes from the presentation policy plus
/// these stable maxima alone — never from the rule count or the message
/// length — so empty/populated/pending/error frames share one `frame` and
/// sticky tail origins. `body_content_rows` sizes only the list's shared
/// scroll extent; rule parameters are owned by the child editor.
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

    fn controls(&self, ctx: &Ctx<'_>) -> Vec<ColorRulesControl> {
        let rules = Self::draft(ctx).len();
        let mut controls = vec![ColorRulesControl::List];
        controls.push(ColorRulesControl::Add);
        if rules > 0 {
            controls.push(ColorRulesControl::Edit);
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
            ColorRulesControl::Edit => Some(1),
            ColorRulesControl::Remove => Some(2),
            ColorRulesControl::Apply => Some(3),
            _ => None,
        }
    }

    fn move_selection(&mut self, delta: i32, ctx: &mut Ctx<'_>) {
        let rules = Self::draft(ctx).len();
        if rules == 0 {
            return;
        }
        self.selected = (self.selected as i32 + delta).rem_euclid(rules as i32) as usize;
    }

    fn add(&mut self, ctx: &mut Ctx<'_>) -> Outcome {
        if Self::draft(ctx).len() >= MAX_COLOR_RULES {
            Self::error(
                ctx,
                &format!("at most {MAX_COLOR_RULES} colour rules in one view"),
            );
            return Outcome::Consumed;
        }
        Outcome::OpenChild(crate::component::Open::ColorRuleEditor { editing: None })
    }

    fn edit(&mut self, ctx: &mut Ctx<'_>) -> Outcome {
        if Self::draft(ctx).get(self.selected).is_none() {
            ctx.notice("no colour rule is selected; use Add to create one");
            return Outcome::Consumed;
        }
        Outcome::OpenChild(crate::component::Open::ColorRuleEditor {
            editing: Some(self.selected),
        })
    }

    fn remove(&mut self, ctx: &mut Ctx<'_>) {
        let selected = self.selected;
        if selected >= Self::draft(ctx).len() {
            return;
        }
        Self::with_draft(ctx, |rules| {
            rules.remove(selected);
        });
        self.selected = selected.min(Self::draft(ctx).len().saturating_sub(1));
        if Self::draft(ctx).is_empty() {
            self.control = ColorRulesControl::List;
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
            ColorRulesControl::Add => self.add(ctx),
            ColorRulesControl::Edit => self.edit(ctx),
            ColorRulesControl::Remove => {
                self.remove(ctx);
                Outcome::Consumed
            }
            ColorRulesControl::List => {
                if Self::draft(ctx).is_empty() {
                    self.add(ctx)
                } else {
                    self.edit(ctx)
                }
            }
            ColorRulesControl::Apply => self.apply(ctx),
            // Parameter controls belong exclusively to ColorRuleEditor. These
            // retained public variants keep downstream API compatibility but
            // are never traversed by the manager.
            ColorRulesControl::Column | ColorRulesControl::Predicate | ColorRulesControl::Color => {
                Outcome::Ignored
            }
        }
    }

    fn key(&mut self, key: KeyEvent, ctx: &mut Ctx<'_>) -> Outcome {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return Outcome::Ignored;
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
            KeyCode::Up => {
                self.move_selection(-1, ctx);
                Outcome::Consumed
            }
            KeyCode::Down => {
                self.move_selection(1, ctx);
                Outcome::Consumed
            }
            KeyCode::Enter => self.activate(ctx),
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
                self.control = ColorRulesControl::List;
                return Outcome::Consumed;
            }
            (true, Some(ColorRulesHit::Row(index))) => {
                self.selected = index.min(Self::draft(ctx).len().saturating_sub(1));
                self.control = ColorRulesControl::List;
                return Outcome::Consumed;
            }
            (true, Some(ColorRulesHit::Control(control))) => {
                if self.controls(ctx).contains(&control) {
                    self.control = control;
                }
                if control != ColorRulesControl::List {
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
    }

    fn handle(&mut self, event: Event<ColorRulesHit>, ctx: &mut Ctx<'_>) -> Outcome {
        match event {
            Event::Key(key) => self.key(key, ctx),
            Event::Paste(_) => Outcome::Ignored,
            Event::Mouse { kind, hit, .. } => self.mouse(kind, hit, ctx),
            Event::Dismiss => {
                // §10 frontmost-first: the overflow menu closes before the
                // dialog it hangs from.
                if self.more_open {
                    self.more_open = false;
                    return Outcome::Consumed;
                }
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
        let mut labels = COLOR_RULES_BUTTONS.to_vec();
        if self.geometry.more_button.is_some() {
            labels.push(crate::dialog_controls::MORE_LABEL);
        }
        labels
    }

    fn press_action(&mut self, index: usize, ctx: &mut Ctx<'_>) -> Outcome {
        if index == COLOR_RULES_BUTTONS.len() {
            if self.more_open {
                self.more_open = false;
            } else {
                self.open_more(None);
            }
            return Outcome::Consumed;
        }
        // Any press dismisses the transient overflow menu first — including
        // the menu's own items, which arrive here with original indices (0
        // Add, 1 Edit, 2 Remove, 3 Apply) and run exactly what their buttons would.
        self.more_open = false;
        match index {
            0 => return self.add(ctx),
            1 => return self.edit(ctx),
            2 => self.remove(ctx),
            3 => return self.apply(ctx),
            _ => return Outcome::Ignored,
        }
        Outcome::Consumed
    }

    fn surface(&self) -> Surface {
        Surface {
            text_focus: false,
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
        // The manager is only an object list. Parameter rows belong to the
        // child editor, so selecting a rule cannot silently expose or mutate
        // its column, value, predicate, or colour.
        let rule_rows = rules.len().max(1);
        let content_rows = 1usize.saturating_add(rule_rows);
        let spec = color_spec(area);
        let Ok(geometry) = crate::dialog_layout::resolve_dialog(
            area,
            &spec,
            content_rows,
            &labels,
            Some(if rules.is_empty() { 0 } else { 1 }),
            ctx.context_anchor,
        ) else {
            // Below the 20x6 floor the tiny fallback owns the frame; stay open
            // with nothing drawn, as the palette does.
            self.geometry = ColorRulesGeometry::default();
            self.surface = Surface::default();
            return self.surface;
        };
        let title = ctx.views.active_item().map_or_else(
            || "Colour rules".to_owned(),
            |view| format!("View · {} › Colour rules", view.name),
        );
        render_responsive_frame(frame, &geometry, &title, true, theme);
        // One authoritative geometry for frame/anatomy/body/actions. The same
        // projected rects drive paint, caret, scrollbar, selection and mouse;
        // no independent outer calculation.
        let mut surface = Surface {
            popup: geometry.frame,
            interior: geometry.interior,
            caret: None,
            scrollable: geometry.body.overflow() > 0,
            text_focus: false,
        };
        self.geometry.body = geometry.body.viewport;

        // Shared body projection keeps the selected object visible. There is
        // no parameter focus in this layer.
        let mut body = ScrollViewport::new(geometry.body.viewport, content_rows, 0);
        if !rules.is_empty() {
            let selected_row = 1usize.saturating_add(self.selected.min(rules.len() - 1));
            body = ScrollViewport::new(
                geometry.body.viewport,
                content_rows,
                body.reveal(selected_row),
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

        render_message(frame, geometry.message, state, &sentence, theme, ascii);
        render_help_text(frame, geometry.help, help, theme);
        // §8.9: the verb is the default, and a destructive action never is.
        // With nothing to apply yet the fill moves to `Add`, the only button
        // that does anything on an empty list. Roles are presentation only;
        // the rects come from the one shared action geometry, so click and
        // paint cannot disagree.
        let default_idx = if rules.is_empty() { 0 } else { 1 };
        let focused_idx = match self.control {
            ColorRulesControl::Add => Some(0),
            ColorRulesControl::Edit => Some(1),
            ColorRulesControl::Remove => Some(2),
            ColorRulesControl::Apply => Some(3),
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
                1 => ColorRulesControl::Edit,
                2 => ColorRulesControl::Remove,
                _ => ColorRulesControl::Apply,
            };
            if let Some(label) = labels.get(*index) {
                let role = if *index == 2 {
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
                    if rules.is_empty() {
                        ButtonRole::Normal
                    } else {
                        ButtonRole::Default
                    },
                    ButtonRole::Destructive,
                    ButtonRole::Normal,
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
        surface.caret = None;
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

/// Parameters for the rule-parameter child. `None` creates a new rule;
/// `Some(index)` edits exactly the manager row that opened it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ColorRuleEditorOpen {
    pub editing: Option<usize>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ColorRuleEditorControl {
    Column,
    #[default]
    Value,
    Color,
    Save,
    Cancel,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ColorRuleEditorHit {
    Control(ColorRuleEditorControl),
    Body,
}

#[derive(Clone, Debug, Default)]
struct ColorRuleEditorGeometry {
    body: Rect,
    controls: Vec<(Rect, ColorRuleEditorControl)>,
}

/// A true child editor. Its rule is local until Save, so Escape cannot leak a
/// half-entered parameter into the manager's per-view draft.
#[derive(Debug, Default)]
pub struct ColorRuleEditorDialog {
    open: bool,
    view_id: String,
    editing: Option<usize>,
    rule: ColorRule,
    columns: Vec<String>,
    control: ColorRuleEditorControl,
    cursor: TextCursor,
    error: Option<String>,
    geometry: ColorRuleEditorGeometry,
    surface: Surface,
}

const COLOR_RULE_EDITOR_BUTTONS: [&str; 2] = ["&Save", "&Cancel"];

impl ColorRuleEditorDialog {
    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn editing(&self) -> Option<usize> {
        self.editing
    }

    pub fn control(&self) -> ColorRuleEditorControl {
        self.control
    }

    pub fn rule(&self) -> &ColorRule {
        &self.rule
    }

    pub fn control_rects(&self) -> &[(Rect, ColorRuleEditorControl)] {
        &self.geometry.controls
    }

    fn text(&self) -> &str {
        if self.rule.is_column() {
            self.rule.value.as_deref().unwrap_or_default()
        } else {
            &self.rule.predicate
        }
    }

    fn text_editing(&self) -> bool {
        self.control == ColorRuleEditorControl::Value
    }

    fn reset_cursor(&mut self) {
        let text = self.text().to_owned();
        reset_cursor_to_end(&text, &mut self.cursor);
    }

    fn controls(&self) -> Vec<ColorRuleEditorControl> {
        let mut controls = Vec::new();
        if self.rule.is_column() {
            controls.push(ColorRuleEditorControl::Column);
        }
        controls.extend([
            ColorRuleEditorControl::Value,
            ColorRuleEditorControl::Color,
            ColorRuleEditorControl::Save,
            ColorRuleEditorControl::Cancel,
        ]);
        controls
    }

    fn move_control(&mut self, delta: i32) -> Outcome {
        let controls = self.controls();
        let at = controls
            .iter()
            .position(|control| *control == self.control)
            .unwrap_or(0);
        self.control = controls[(at as i32 + delta).rem_euclid(controls.len() as i32) as usize];
        if self.text_editing() {
            self.reset_cursor();
        }
        Outcome::Consumed
    }

    fn cycle_column(&mut self, delta: i32) -> Outcome {
        if self.columns.is_empty() || !self.rule.is_column() {
            return Outcome::Consumed;
        }
        let current = self.rule.column.as_deref().unwrap_or_default();
        let at = self
            .columns
            .iter()
            .position(|name| name == current)
            .unwrap_or(if delta >= 0 {
                self.columns.len() - 1
            } else {
                0
            });
        let next = (at as i32 + delta).rem_euclid(self.columns.len() as i32) as usize;
        self.rule.column = Some(self.columns[next].clone());
        self.error = None;
        Outcome::Consumed
    }

    fn cycle_color(&mut self, delta: i32) -> Outcome {
        let at = RuleColor::ALL
            .iter()
            .position(|color| *color == self.rule.color)
            .unwrap_or(0);
        let next = (at as i32 + delta).rem_euclid(RuleColor::ALL.len() as i32) as usize;
        self.rule.color = RuleColor::ALL[next];
        self.error = None;
        Outcome::Consumed
    }

    fn edit_text(&mut self, command: EditCommand<'_>) -> Outcome {
        if !self.text_editing() {
            return Outcome::Ignored;
        }
        let column = self.rule.is_column();
        let mut value = self.text().to_owned();
        let mut cursor = self.cursor;
        let changed = edit(
            &mut value,
            &mut cursor,
            command,
            EditPolicy {
                max_bytes: MAX_PREDICATE_BYTES,
                multiline: false,
            },
        );
        self.cursor = cursor;
        if changed.changed {
            if column {
                self.rule.value = Some(value);
            } else {
                self.rule.predicate = value;
            }
            self.error = None;
        }
        if changed.changed || changed.moved {
            Outcome::Consumed
        } else {
            Outcome::Ignored
        }
    }

    fn save(&mut self, ctx: &mut Ctx<'_>) -> Outcome {
        if let Err(error) = ColorRulesDialog::validate(std::slice::from_ref(&self.rule)) {
            self.error = Some(error);
            return Outcome::Consumed;
        }
        let Some(state) = ctx.views.state_mut(&self.view_id) else {
            self.error = Some("the view is no longer available".to_owned());
            return Outcome::Consumed;
        };
        match self.editing {
            Some(index) => {
                let Some(slot) = state.color_rules_draft.get_mut(index) else {
                    self.error = Some("the selected rule is no longer available".to_owned());
                    return Outcome::Consumed;
                };
                *slot = self.rule.clone();
            }
            None if state.color_rules_draft.len() < MAX_COLOR_RULES => {
                state.color_rules_draft.push(self.rule.clone());
            }
            None => {
                self.error = Some(format!(
                    "at most {MAX_COLOR_RULES} colour rules in one view"
                ));
                return Outcome::Consumed;
            }
        }
        state.color_rules_error = None;
        ctx.views.touch(&self.view_id);
        self.open = false;
        Outcome::Close
    }

    fn cancel(&mut self) -> Outcome {
        self.open = false;
        Outcome::Close
    }

    fn activate(&mut self, ctx: &mut Ctx<'_>) -> Outcome {
        match self.control {
            ColorRuleEditorControl::Column | ColorRuleEditorControl::Color => {
                if self.control == ColorRuleEditorControl::Column {
                    self.cycle_column(1)
                } else {
                    self.cycle_color(1)
                }
            }
            ColorRuleEditorControl::Value | ColorRuleEditorControl::Save => self.save(ctx),
            ColorRuleEditorControl::Cancel => self.cancel(),
        }
    }

    fn key(&mut self, key: KeyEvent, ctx: &mut Ctx<'_>) -> Outcome {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return Outcome::Ignored;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return match key.code {
                KeyCode::Char('a') => self.edit_text(EditCommand::StartOfLine),
                KeyCode::Char('e') => self.edit_text(EditCommand::EndOfLine),
                KeyCode::Char('k') => self.edit_text(EditCommand::KillToEndOfLine),
                _ => Outcome::Ignored,
            };
        }
        match key.code {
            KeyCode::BackTab => self.move_control(-1),
            KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => self.move_control(-1),
            KeyCode::Tab => self.move_control(1),
            KeyCode::Left if self.control == ColorRuleEditorControl::Column => {
                self.cycle_column(-1)
            }
            KeyCode::Right if self.control == ColorRuleEditorControl::Column => {
                self.cycle_column(1)
            }
            KeyCode::Left if self.control == ColorRuleEditorControl::Color => self.cycle_color(-1),
            KeyCode::Right if self.control == ColorRuleEditorControl::Color => self.cycle_color(1),
            KeyCode::Left => self.edit_text(EditCommand::MoveLeft),
            KeyCode::Right => self.edit_text(EditCommand::MoveRight),
            KeyCode::Backspace => self.edit_text(EditCommand::Backspace),
            KeyCode::Enter => self.activate(ctx),
            KeyCode::Char(character) => {
                let mut buffer = [0u8; 4];
                self.edit_text(EditCommand::Insert(character.encode_utf8(&mut buffer)))
            }
            _ => Outcome::Ignored,
        }
    }

    fn mouse(
        &mut self,
        kind: MouseEventKind,
        hit: Option<ColorRuleEditorHit>,
        ctx: &mut Ctx<'_>,
    ) -> Outcome {
        if !matches!(kind, MouseEventKind::Down(MouseButton::Left)) {
            return Outcome::Ignored;
        }
        let Some(ColorRuleEditorHit::Control(control)) = hit else {
            return Outcome::Consumed;
        };
        if !self.controls().contains(&control) {
            return Outcome::Consumed;
        }
        self.control = control;
        if self.text_editing() {
            self.reset_cursor();
        }
        match control {
            ColorRuleEditorControl::Column
            | ColorRuleEditorControl::Color
            | ColorRuleEditorControl::Save
            | ColorRuleEditorControl::Cancel => self.activate(ctx),
            ColorRuleEditorControl::Value => Outcome::Consumed,
        }
    }
}

impl Component for ColorRuleEditorDialog {
    type Hit = ColorRuleEditorHit;
    type Open = ColorRuleEditorOpen;

    fn open(&mut self, params: ColorRuleEditorOpen, ctx: &mut Ctx<'_>) {
        self.open = true;
        self.view_id = ctx.views.active_id().unwrap_or_default().to_owned();
        self.editing = params.editing;
        self.columns = ColorRulesDialog::classifiable_columns(ctx);
        self.error = None;
        self.geometry = ColorRuleEditorGeometry::default();
        self.surface = Surface::default();
        self.rule = params
            .editing
            .and_then(|index| {
                ctx.views
                    .state(&self.view_id)
                    .and_then(|state| state.color_rules_draft.get(index))
                    .cloned()
            })
            .unwrap_or_else(|| {
                let count = ctx
                    .views
                    .state(&self.view_id)
                    .map_or(0, |state| state.color_rules_draft.len());
                let color = RuleColor::ALL[count % RuleColor::ALL.len()];
                self.columns.first().cloned().map_or_else(
                    || ColorRule::predicate_rule(String::new(), color),
                    |column| ColorRule::column_rule(column, String::new(), color),
                )
            });
        self.control = ColorRuleEditorControl::Value;
        self.reset_cursor();
    }

    fn handle(&mut self, event: Event<Self::Hit>, ctx: &mut Ctx<'_>) -> Outcome {
        match event {
            Event::Key(key) => self.key(key, ctx),
            Event::Paste(text) => self.edit_text(EditCommand::Insert(&text)),
            Event::Mouse { kind, hit, .. } => self.mouse(kind, hit, ctx),
            Event::Dismiss => self.cancel(),
            Event::Command(_) | Event::View(_) | Event::Resize => Outcome::Ignored,
        }
    }

    fn action_labels(&self, _ctx: &Ctx<'_>) -> Vec<&'static str> {
        COLOR_RULE_EDITOR_BUTTONS.to_vec()
    }

    fn press_action(&mut self, index: usize, ctx: &mut Ctx<'_>) -> Outcome {
        match index {
            0 => self.save(ctx),
            1 => self.cancel(),
            _ => Outcome::Ignored,
        }
    }

    fn hit(&self, point: (u16, u16)) -> Option<Self::Hit> {
        self.geometry
            .controls
            .iter()
            .find_map(|(rect, control)| {
                contains(*rect, point).then_some(ColorRuleEditorHit::Control(*control))
            })
            .or_else(|| contains(self.geometry.body, point).then_some(ColorRuleEditorHit::Body))
    }

    fn surface(&self) -> Surface {
        Surface {
            text_focus: self.text_editing(),
            ..self.surface
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx<'_>) -> Surface {
        let theme = ctx.theme;
        let styles = DialogStyles::new(theme);
        let child_area = if crate::dialog_layout::is_compact(area) {
            area
        } else {
            Rect::new(
                area.x.saturating_add(6),
                area.y.saturating_add(1),
                area.width.saturating_sub(12),
                area.height.saturating_sub(2),
            )
        };
        let body_rows = if self.rule.is_column() { 3 } else { 2 };
        let (policy_width, _) =
            crate::dialog_layout::policy_size(child_area, PresentationKind::SelfContainedForm);
        let action_rows = crate::dialog_controls::stable_action_rows(
            policy_width.saturating_sub(4).max(1),
            &COLOR_RULE_EDITOR_BUTTONS,
        );
        let spec = DialogSpec::new(PresentationKind::SelfContainedForm, 0, 1, 2, 1, action_rows);
        let Ok(geometry) = crate::dialog_layout::resolve_dialog(
            child_area,
            &spec,
            body_rows,
            &COLOR_RULE_EDITOR_BUTTONS,
            Some(0),
            None,
        ) else {
            self.geometry = ColorRuleEditorGeometry::default();
            self.surface = Surface::default();
            return self.surface;
        };
        let title = self.editing.map_or_else(
            || "New colour rule".to_owned(),
            |index| format!("Rule · {} › Edit", index + 1),
        );
        render_responsive_frame(frame, &geometry, &title, true, theme);

        let label_width = 9u16.min(geometry.body.viewport.width);
        let field_x = geometry
            .body
            .viewport
            .x
            .saturating_add(label_width)
            .saturating_add(FIELD_GUTTER);
        let mut controls = Vec::new();
        let mut row = 0usize;
        if self.rule.is_column() {
            if let Some(rect) =
                ScrollViewport::new(geometry.body.viewport, body_rows, 0).project_row(row)
            {
                frame.render_widget(
                    Paragraph::new("Column").style(
                        if self.control == ColorRuleEditorControl::Column {
                            styles.shortcut.add_modifier(Modifier::BOLD)
                        } else {
                            styles.label
                        },
                    ),
                    Rect::new(rect.x, rect.y, label_width.min(rect.width), 1),
                );
                if field_x < rect.right() {
                    let field = Rect::new(field_x, rect.y, rect.right() - field_x, 1);
                    let column = self.rule.column.as_deref().unwrap_or_default();
                    frame.render_widget(
                        Paragraph::new(truncated(
                            &format!("‹ {column} ›"),
                            usize::from(field.width),
                        ))
                        .style(styles.description),
                        field,
                    );
                    controls.push((rect, ColorRuleEditorControl::Column));
                }
            }
            row += 1;
        }
        let viewport = ScrollViewport::new(geometry.body.viewport, body_rows, 0);
        let mut caret = None;
        if let Some(rect) = viewport.project_row(row) {
            let label = if self.rule.is_column() {
                "Value"
            } else {
                "Predicate"
            };
            frame.render_widget(
                Paragraph::new(label).style(if self.text_editing() {
                    styles.shortcut.add_modifier(Modifier::BOLD)
                } else {
                    styles.label
                }),
                Rect::new(rect.x, rect.y, label_width.min(rect.width), 1),
            );
            if field_x < rect.right() {
                let field = Rect::new(field_x, rect.y, rect.right() - field_x, 1);
                if self.text_editing() {
                    caret = place_input_cursor_at(
                        frame,
                        field,
                        0,
                        0,
                        self.text(),
                        self.cursor.char_index,
                        theme,
                    );
                } else {
                    frame.render_widget(
                        Paragraph::new(truncated(self.text(), usize::from(field.width)))
                            .style(styles.description),
                        field,
                    );
                }
                controls.push((rect, ColorRuleEditorControl::Value));
            }
        }
        row += 1;
        if let Some(rect) = viewport.project_row(row) {
            frame.render_widget(
                Paragraph::new("Colour").style(if self.control == ColorRuleEditorControl::Color {
                    styles.shortcut.add_modifier(Modifier::BOLD)
                } else {
                    styles.label
                }),
                Rect::new(rect.x, rect.y, label_width.min(rect.width), 1),
            );
            if field_x < rect.right() {
                let field = Rect::new(field_x, rect.y, rect.right() - field_x, 1);
                frame.render_widget(
                    Paragraph::new(truncated(
                        &format!("‹ {} ›", self.rule.color.label()),
                        usize::from(field.width),
                    ))
                    .style(styles.description.fg(theme.rule_color(self.rule.color))),
                    field,
                );
                controls.push((rect, ColorRuleEditorControl::Color));
            }
        }

        let (state, sentence) = self.error.as_ref().map_or(
            (
                MessageState::Ready,
                if self.editing.is_some() {
                    "Save replaces this rule in the editable list".to_owned()
                } else {
                    "Save adds this rule to the editable list".to_owned()
                },
            ),
            |error| (MessageState::Error, error.clone()),
        );
        render_message(frame, geometry.message, state, &sentence, theme, ctx.ascii);
        render_help_text(
            frame,
            geometry.help,
            "Choose parameters here; Apply remains an operation in the rule manager.",
            theme,
        );
        for (index, rect) in &geometry.actions.buttons {
            let control = if *index == 0 {
                ColorRuleEditorControl::Save
            } else {
                ColorRuleEditorControl::Cancel
            };
            render_role_button(
                frame,
                *rect,
                COLOR_RULE_EDITOR_BUTTONS[*index],
                if *index == 0 {
                    ButtonRole::Default
                } else {
                    ButtonRole::Normal
                },
                self.control == control,
                theme,
            );
            controls.push((*rect, control));
        }
        let surface = Surface {
            popup: geometry.frame,
            interior: geometry.interior,
            caret,
            scrollable: false,
            text_focus: self.text_editing(),
        };
        self.geometry = ColorRuleEditorGeometry {
            body: geometry.body.viewport,
            controls,
        };
        self.surface = surface;
        surface
    }
}
