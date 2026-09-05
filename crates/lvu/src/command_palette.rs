//! Bounded command catalog and palette state. The terminal integration owns the
//! global toggle and passes a snapshot of application availability to `open`.

use crate::app::{
    Action, AskAiKind, Focus, RecipeDialogMode, SourceKind, TimeBasis, ViewDialogMode,
    key_to_action,
};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph},
};

use crate::theme::Theme;

pub const MAX_QUERY_BYTES: usize = 256;
pub const MAX_RESULTS: usize = 64;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum CommandId {
    AddSource,
    DiscoverSources,
    AskAiSource,
    SourceFileMode,
    SourceCommandMode,
    LiteralFilter,
    AdvancedFilter,
    Enrichment,
    EnrichmentAdd,
    EnrichmentEdit,
    EnrichmentRemove,
    EditorCompletion,
    Grouping,
    ToggleExpandedGroup,
    NextView,
    PreviousView,
    TimeWindow,
    TimeClear,
    TimeAroundSelected,
    TimeBasisCapture,
    TimeBasisEvent,
    TimeBasisExtracted,
    TimeRecentFive,
    TimeRecentFifteen,
    TimeRecentHour,
    ViewDialog,
    ViewBlank,
    ViewClone,
    ViewRename,
    Recipes,
    RecipeBrowse,
    RecipeSave,
    RecipeImport,
    RecipeApply,
    RecipeRefreshSuggestions,
    RecipeAdaptSuggestion,
    RecipeRejectSuggestion,
    Fields,
    PinField,
    ColorField,
    ScrollLogLeft,
    ScrollLogRight,
    ResetLogHorizontal,
    Follow,
    Details,
    StoragePreview,
    StorageClear,
    Settings,
    TimestampAssistant,
    AskAi,
    AskAiFilter,
    AskAiEnrichment,
    Investigations,
    NewInvestigation,
    ResumeInvestigation,
    InvestigationFollowup,
    Help,
    Quit,
}

pub const REQUIRED_COMMANDS: &[CommandId] = &[
    CommandId::AddSource,
    CommandId::DiscoverSources,
    CommandId::AskAiSource,
    CommandId::SourceFileMode,
    CommandId::SourceCommandMode,
    CommandId::LiteralFilter,
    CommandId::AdvancedFilter,
    CommandId::Enrichment,
    CommandId::EnrichmentAdd,
    CommandId::EnrichmentEdit,
    CommandId::EnrichmentRemove,
    CommandId::EditorCompletion,
    CommandId::Grouping,
    CommandId::ToggleExpandedGroup,
    CommandId::NextView,
    CommandId::PreviousView,
    CommandId::TimeWindow,
    CommandId::TimeClear,
    CommandId::TimeAroundSelected,
    CommandId::TimeBasisCapture,
    CommandId::TimeBasisEvent,
    CommandId::TimeBasisExtracted,
    CommandId::TimeRecentFive,
    CommandId::TimeRecentFifteen,
    CommandId::TimeRecentHour,
    CommandId::ViewDialog,
    CommandId::ViewBlank,
    CommandId::ViewClone,
    CommandId::ViewRename,
    CommandId::Recipes,
    CommandId::RecipeBrowse,
    CommandId::RecipeSave,
    CommandId::RecipeImport,
    CommandId::RecipeApply,
    CommandId::RecipeRefreshSuggestions,
    CommandId::RecipeAdaptSuggestion,
    CommandId::RecipeRejectSuggestion,
    CommandId::Fields,
    CommandId::PinField,
    CommandId::ColorField,
    CommandId::ScrollLogLeft,
    CommandId::ScrollLogRight,
    CommandId::ResetLogHorizontal,
    CommandId::Follow,
    CommandId::Details,
    CommandId::StoragePreview,
    CommandId::StorageClear,
    CommandId::Settings,
    CommandId::TimestampAssistant,
    CommandId::AskAi,
    CommandId::AskAiFilter,
    CommandId::AskAiEnrichment,
    CommandId::Investigations,
    CommandId::NewInvestigation,
    CommandId::ResumeInvestigation,
    CommandId::InvestigationFollowup,
    CommandId::Help,
    CommandId::Quit,
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PaletteContext {
    pub focus: Focus,
    pub has_view: bool,
    pub has_selected_row: bool,
    pub storage_confirmation_ready: bool,
    pub recipe_mode: Option<RecipeDialogMode>,
    pub investigation_can_resume: bool,
    pub investigation_can_follow_up: bool,
}

impl PaletteContext {
    pub const fn new(focus: Focus, has_view: bool) -> Self {
        Self {
            focus,
            has_view,
            has_selected_row: false,
            storage_confirmation_ready: false,
            recipe_mode: None,
            investigation_can_resume: false,
            investigation_can_follow_up: false,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Command {
    pub id: CommandId,
    pub name: &'static str,
    pub description: &'static str,
    pub category: &'static str,
    pub aliases: &'static [&'static str],
    pub action: Action,
    pub shortcut: Option<&'static str>,
    pub unavailable_reason: Option<&'static str>,
}

impl Command {
    pub fn is_enabled(&self) -> bool {
        self.unavailable_reason.is_none()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PaletteOutcome {
    None,
    Execute(Action),
    Closed { restore_focus: Focus },
}

#[derive(Clone, Debug)]
pub struct Palette {
    open: bool,
    return_focus: Focus,
    context: PaletteContext,
    query: String,
    commands: Vec<Command>,
    matches: Vec<usize>,
    selected: usize,
    scroll: usize,
    visible_rows: usize,
    rows: Vec<(Rect, usize)>,
}

impl Default for Palette {
    fn default() -> Self {
        Self::new()
    }
}

impl Palette {
    pub fn new() -> Self {
        let context = PaletteContext::new(Focus::Logs, false);
        let mut palette = Self {
            open: false,
            return_focus: Focus::Logs,
            context,
            query: String::new(),
            commands: Vec::new(),
            matches: Vec::new(),
            selected: 0,
            scroll: 0,
            visible_rows: 0,
            rows: Vec::new(),
        };
        palette.replace_catalog();
        palette
    }

    pub fn open(&mut self, context: PaletteContext) {
        self.open = true;
        self.return_focus = context.focus;
        self.context = context;
        self.query.clear();
        self.selected = 0;
        self.scroll = 0;
        self.replace_catalog();
    }

    /// Recomputes availability without discarding the query. `handle_key`
    /// requires current context and calls this before Enter can execute.
    pub fn refresh_context(&mut self, context: PaletteContext) {
        if self.context == context {
            return;
        }
        let selected = self.selected_command().map(|command| command.id);
        self.context = context;
        self.replace_catalog();
        if let Some(selected) = selected
            && let Some(index) = self
                .matches
                .iter()
                .position(|entry| self.commands[*entry].id == selected)
        {
            self.selected = index;
            self.keep_selected_visible();
        }
    }

    pub fn close(&mut self) -> PaletteOutcome {
        self.open = false;
        PaletteOutcome::Closed {
            restore_focus: self.return_focus,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn context(&self) -> PaletteContext {
        self.context
    }

    pub fn commands(&self) -> &[Command] {
        &self.commands
    }

    pub fn results(&self) -> impl Iterator<Item = &Command> {
        self.matches.iter().map(|index| &self.commands[*index])
    }

    pub fn selected_command(&self) -> Option<&Command> {
        self.matches
            .get(self.selected)
            .map(|index| &self.commands[*index])
    }

    pub fn is_toggle_key(key: KeyEvent) -> bool {
        matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
            && key.code == KeyCode::Char('p')
            && key.modifiers.contains(KeyModifiers::CONTROL)
    }

    pub fn handle_key(&mut self, key: KeyEvent, current_context: PaletteContext) -> PaletteOutcome {
        if !self.open || !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return PaletteOutcome::None;
        }
        self.refresh_context(current_context);
        if Self::is_toggle_key(key) || key.code == KeyCode::Esc {
            return self.close();
        }
        match key.code {
            KeyCode::Up => self.move_selection(-1),
            KeyCode::Down => self.move_selection(1),
            KeyCode::PageUp => self.move_selection(-(self.visible_rows.max(1) as isize)),
            KeyCode::PageDown => self.move_selection(self.visible_rows.max(1) as isize),
            KeyCode::Home => self.set_selection(0),
            KeyCode::End => self.set_selection(self.matches.len().saturating_sub(1)),
            KeyCode::Backspace => {
                self.query.pop();
                self.refresh_matches();
            }
            KeyCode::Tab => {
                if let Some(name) = self.selected_command().map(|command| command.name) {
                    self.query.clear();
                    self.push_bounded(name);
                    self.refresh_matches();
                }
            }
            KeyCode::Enter => {
                let action = self
                    .selected_command()
                    .filter(|command| command.is_enabled())
                    .map(|command| command.action.clone());
                if let Some(action) = action {
                    self.open = false;
                    return PaletteOutcome::Execute(action);
                }
            }
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.push_bounded(&character.to_string());
                self.refresh_matches();
            }
            _ => {}
        }
        PaletteOutcome::None
    }

    pub fn handle_mouse(&mut self, mouse: MouseEvent) -> PaletteOutcome {
        if !self.open {
            return PaletteOutcome::None;
        }
        match mouse.kind {
            MouseEventKind::ScrollUp => self.move_selection(-1),
            MouseEventKind::ScrollDown => self.move_selection(1),
            MouseEventKind::Down(_) => {
                if let Some((_, result)) = self.rows.iter().find(|(area, _)| {
                    mouse.column >= area.x
                        && mouse.column < area.right()
                        && mouse.row >= area.y
                        && mouse.row < area.bottom()
                }) {
                    self.set_selection(*result);
                }
            }
            _ => {}
        }
        PaletteOutcome::None
    }

    pub fn handle_paste(&mut self, text: &str) {
        if self.open {
            self.push_bounded(text);
            self.refresh_matches();
        }
    }

    pub fn resize(&mut self, area: Rect) {
        self.visible_rows = area.height.saturating_sub(4) as usize;
        self.keep_selected_visible();
    }

    pub fn render(&mut self, frame: &mut Frame<'_>, area: Rect) {
        self.render_with_theme(frame, area, Theme::TERMINAL);
    }

    pub fn render_with_theme(&mut self, frame: &mut Frame<'_>, area: Rect, theme: Theme) {
        self.rows.clear();
        if !self.open || area.width < 4 || area.height < 3 {
            return;
        }
        let width = area.width.min(92);
        let height = area.height.min(24);
        let popup = Rect::new(
            area.x + area.width.saturating_sub(width) / 2,
            area.y + area.height.saturating_sub(height) / 2,
            width,
            height,
        );
        frame.render_widget(Clear, popup);
        let block = Block::default()
            .title(" Command palette · Ctrl-P ")
            .borders(Borders::ALL)
            .style(Style::default().fg(theme.base_fg).bg(theme.base_bg))
            .border_style(Style::default().fg(theme.active_border));
        let inner = block.inner(popup);
        frame.render_widget(block, popup);
        if inner.height == 0 {
            return;
        }
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(inner);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("> ", Style::default().fg(theme.accent)),
                Span::raw(self.query.as_str()),
            ])),
            chunks[0],
        );
        self.visible_rows = chunks[1].height as usize;
        self.keep_selected_visible();
        let end = (self.scroll + self.visible_rows).min(self.matches.len());
        let mut items = Vec::with_capacity(end.saturating_sub(self.scroll));
        for (screen_row, result_index) in (self.scroll..end).enumerate() {
            let command = &self.commands[self.matches[result_index]];
            let selected = result_index == self.selected;
            let prefix = if selected { "› " } else { "  " };
            let shortcut = command.shortcut.unwrap_or("");
            let suffix = command
                .unavailable_reason
                .map(|reason| format!(" — unavailable: {reason}"))
                .unwrap_or_default();
            let line = format!(
                "{prefix}{}  {shortcut} [{}]  {}{suffix}",
                command.name, command.category, command.description
            );
            let style = if selected {
                Style::default()
                    .fg(theme.selection_fg)
                    .bg(theme.selection_bg)
                    .add_modifier(Modifier::BOLD)
            } else if !command.is_enabled() {
                Style::default().fg(theme.muted)
            } else {
                Style::default().fg(theme.base_fg).bg(theme.base_bg)
            };
            items.push(ListItem::new(line).style(style));
            self.rows.push((
                Rect::new(
                    chunks[1].x,
                    chunks[1].y + screen_row as u16,
                    chunks[1].width,
                    1,
                ),
                result_index,
            ));
        }
        if items.is_empty() {
            items.push(ListItem::new("  No matching commands"));
        }
        frame.render_widget(List::new(items), chunks[1]);
    }

    fn push_bounded(&mut self, text: &str) {
        for character in text.chars() {
            if self.query.len() + character.len_utf8() > MAX_QUERY_BYTES {
                break;
            }
            self.query.push(character);
        }
    }

    fn move_selection(&mut self, delta: isize) {
        if self.matches.is_empty() {
            self.selected = 0;
            return;
        }
        self.selected = self
            .selected
            .saturating_add_signed(delta)
            .min(self.matches.len() - 1);
        self.keep_selected_visible();
    }

    fn set_selection(&mut self, selected: usize) {
        self.selected = selected.min(self.matches.len().saturating_sub(1));
        self.keep_selected_visible();
    }

    fn keep_selected_visible(&mut self) {
        let visible = self.visible_rows.max(1);
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + visible {
            self.scroll = self.selected + 1 - visible;
        }
        self.scroll = self.scroll.min(self.matches.len().saturating_sub(visible));
    }

    fn replace_catalog(&mut self) {
        self.commands = catalog(self.context);
        self.refresh_matches();
    }

    fn refresh_matches(&mut self) {
        let mut scored: Vec<(usize, u32)> = self
            .commands
            .iter()
            .enumerate()
            .filter_map(|(index, command)| score(command, &self.query).map(|score| (index, score)))
            .collect();
        scored.sort_by(|(left_index, left_score), (right_index, right_score)| {
            right_score
                .cmp(left_score)
                .then_with(|| {
                    self.commands[*left_index]
                        .name
                        .cmp(self.commands[*right_index].name)
                })
                .then_with(|| left_index.cmp(right_index))
        });
        scored.truncate(MAX_RESULTS);
        self.matches = scored.into_iter().map(|(index, _)| index).collect();
        self.selected = self.selected.min(self.matches.len().saturating_sub(1));
        self.scroll = 0;
    }
}

fn score(command: &Command, query: &str) -> Option<u32> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Some(if command.is_enabled() { 10 } else { 0 });
    }
    let fields = std::iter::once(command.name)
        .chain(std::iter::once(command.category))
        .chain(command.aliases.iter().copied());
    let mut total = 0;
    for token in query.split_whitespace() {
        let best = fields
            .clone()
            .filter_map(|field| field_score(&field.to_lowercase(), token))
            .max()?;
        total += best;
    }
    Some(total + u32::from(command.is_enabled()))
}

fn field_score(field: &str, needle: &str) -> Option<u32> {
    if field == needle {
        return Some(1_000);
    }
    if field.starts_with(needle) {
        return Some(800 - field.len().saturating_sub(needle.len()).min(100) as u32);
    }
    if field
        .split(|character: char| !character.is_alphanumeric())
        .any(|word| word.starts_with(needle))
    {
        return Some(650);
    }
    if field.contains(needle) {
        return Some(500);
    }
    let mut positions = Vec::new();
    let mut cursor = 0;
    for character in needle.chars() {
        let offset = field[cursor..].find(character)?;
        cursor += offset + character.len_utf8();
        positions.push(cursor);
    }
    let spread = positions
        .last()
        .copied()
        .unwrap_or(0)
        .saturating_sub(positions.first().copied().unwrap_or(0));
    Some(300_u32.saturating_sub(spread.min(250) as u32))
}

fn catalog(context: PaletteContext) -> Vec<Command> {
    let view_reason = (!context.has_view).then_some("open a source first");
    let focus_reason = |focus, reason| (context.focus != focus).then_some(reason);
    let mut commands = vec![
        command(
            CommandId::AddSource,
            "Add source",
            "Open the admitted source dialog",
            "Sources",
            &["new source", "file", "command"],
            Action::OpenSource,
            None,
        ),
        command(
            CommandId::DiscoverSources,
            "Discover recent sources",
            "Choose a remembered or discovered source",
            "Sources",
            &["recent", "docker", "journal"],
            Action::ToggleDiscovery,
            focus_reason(Focus::SourceDialog, "open Add source first"),
        ),
        command(
            CommandId::AskAiSource,
            "Describe source with agent",
            "Draft a source definition for review",
            "Sources",
            &["source ai", "generate source"],
            Action::ToggleSourceAi,
            focus_reason(Focus::SourceDialog, "open Add source first"),
        ),
        command(
            CommandId::SourceFileMode,
            "Use file source",
            "Select file input in the source dialog",
            "Sources",
            &["path", "tail file"],
            Action::SelectSourceKind(SourceKind::File),
            focus_reason(Focus::SourceDialog, "open Add source first"),
        ),
        command(
            CommandId::SourceCommandMode,
            "Use command source",
            "Select command input in the source dialog",
            "Sources",
            &["process", "argv", "shell"],
            Action::SelectSourceKind(SourceKind::Command),
            focus_reason(Focus::SourceDialog, "open Add source first"),
        ),
        command(
            CommandId::LiteralFilter,
            "Literal filter",
            "Edit case-insensitive text search",
            "Filter",
            &["search", "grep", "text"],
            Action::OpenSearch,
            view_reason,
        ),
        command(
            CommandId::AdvancedFilter,
            "Advanced filter",
            "Edit the Polars predicate",
            "Filter",
            &["predicate", "where", "polars"],
            Action::OpenAdvanced,
            view_reason,
        ),
        command(
            CommandId::Enrichment,
            "Enrichment",
            "Open ordered derived-field stages",
            "Filter",
            &["derive", "column", "polars"],
            Action::OpenEnrichment,
            view_reason,
        ),
        command(
            CommandId::EnrichmentAdd,
            "Add enrichment stage",
            "Stage a regex extraction or named Polars expression after accepted stages",
            "Filter",
            &["derive", "append", "regex"],
            Action::AddEnrichment,
            focus_reason(Focus::EnrichmentEditor, "open Enrichment first"),
        ),
        command(
            CommandId::EnrichmentEdit,
            "Edit selected enrichment stage",
            "Edit a stage without changing its stable identity",
            "Filter",
            &["derive", "change", "stage"],
            Action::EditEnrichment,
            focus_reason(Focus::EnrichmentEditor, "open Enrichment first"),
        ),
        command(
            CommandId::EnrichmentRemove,
            "Remove selected enrichment stage",
            "Validate the remaining ordered chain before publishing it",
            "Filter",
            &["derive", "delete", "stage"],
            Action::RemoveEnrichment,
            focus_reason(Focus::EnrichmentEditor, "open Enrichment first"),
        ),
        command(
            CommandId::EditorCompletion,
            "Complete editor field or value",
            "Insert a sampled field expression or lexical string without applying",
            "Filters",
            &["autocomplete", "field picker", "sampled value"],
            Action::ToggleEditorCompletion,
            (!matches!(
                context.focus,
                Focus::AdvancedEditor | Focus::EnrichmentEditor
            ))
            .then_some("open Advanced filter or Enrichment first"),
        ),
        command(
            CommandId::Grouping,
            "Grouping",
            "Edit continuation grouping",
            "Filter",
            &["multiline", "group"],
            Action::OpenGrouping,
            view_reason,
        ),
        command(
            CommandId::ToggleExpandedGroup,
            "Expand or collapse group",
            "Toggle the selected continuation group",
            "View",
            &["stack trace", "multiline", "fold"],
            Action::ToggleExpandedGroup,
            (!context.has_selected_row).then_some("select a grouped row first"),
        ),
        command(
            CommandId::NextView,
            "Next view",
            "Switch to the next source view",
            "Views",
            &["tab next", "switch"],
            Action::NextView,
            view_reason,
        ),
        command(
            CommandId::PreviousView,
            "Previous view",
            "Switch to the previous source view",
            "Views",
            &["tab previous", "switch"],
            Action::PreviousView,
            view_reason,
        ),
        command(
            CommandId::TimeWindow,
            "Time window",
            "Open time range and basis controls",
            "Time",
            &["range", "timestamp"],
            Action::OpenTime,
            view_reason,
        ),
        command(
            CommandId::TimeClear,
            "Clear time window",
            "Remove the applied time constraint",
            "Time",
            &["all time", "reset range"],
            Action::ClearTime,
            focus_reason(Focus::TimeEditor, "open Time window first"),
        ),
        command(
            CommandId::TimeAroundSelected,
            "Time around selected row",
            "Center a time range on the selected record",
            "Time",
            &["around", "context"],
            Action::AroundSelected,
            focus_reason(Focus::TimeEditor, "open Time window first")
                .or((!context.has_selected_row).then_some("select a row first")),
        ),
        command(
            CommandId::TimeBasisCapture,
            "Use capture time",
            "Filter using ingestion timestamps",
            "Time",
            &["received", "arrival"],
            Action::SetTimeBasis(TimeBasis::Capture),
            focus_reason(Focus::TimeEditor, "open Time window first"),
        ),
        command(
            CommandId::TimeBasisEvent,
            "Use event time",
            "Filter using parsed event timestamps",
            "Time",
            &["timestamp", "parsed"],
            Action::SetTimeBasis(TimeBasis::Event),
            focus_reason(Focus::TimeEditor, "open Time window first"),
        ),
        command(
            CommandId::TimeBasisExtracted,
            "Use extracted timestamp_utc",
            "Alt-U: filter using the accepted UTC timestamp enrichment",
            "Time",
            &["timestamp", "enrichment", "derived"],
            Action::SetTimeBasis(TimeBasis::Extracted),
            focus_reason(Focus::TimeEditor, "open Time window first"),
        ),
        command(
            CommandId::TimeRecentFive,
            "Recent 5 minutes",
            "Apply the five-minute preset",
            "Time",
            &["5m", "five"],
            Action::SetRecentTime(300),
            focus_reason(Focus::TimeEditor, "open Time window first"),
        ),
        command(
            CommandId::TimeRecentFifteen,
            "Recent 15 minutes",
            "Apply the fifteen-minute preset",
            "Time",
            &["15m", "quarter hour"],
            Action::SetRecentTime(900),
            focus_reason(Focus::TimeEditor, "open Time window first"),
        ),
        command(
            CommandId::TimeRecentHour,
            "Recent 1 hour",
            "Apply the one-hour preset",
            "Time",
            &["60m", "1h", "hour"],
            Action::SetRecentTime(3600),
            focus_reason(Focus::TimeEditor, "open Time window first"),
        ),
        command(
            CommandId::ViewDialog,
            "Manage views",
            "Open blank, clone, and rename choices",
            "Views",
            &["new view", "copy view"],
            Action::OpenViewDialog,
            view_reason,
        ),
        command(
            CommandId::ViewBlank,
            "Create blank view",
            "Select a blank view in the view dialog",
            "Views",
            &["new empty"],
            Action::SelectViewDialogMode(ViewDialogMode::Blank),
            focus_reason(Focus::ViewDialog, "open Manage views first"),
        ),
        command(
            CommandId::ViewClone,
            "Clone view",
            "Select clone in the view dialog",
            "Views",
            &["duplicate", "copy"],
            Action::SelectViewDialogMode(ViewDialogMode::Clone),
            focus_reason(Focus::ViewDialog, "open Manage views first"),
        ),
        command(
            CommandId::ViewRename,
            "Rename view",
            "Select rename in the view dialog",
            "Views",
            &["name"],
            Action::SelectViewDialogMode(ViewDialogMode::Rename),
            focus_reason(Focus::ViewDialog, "open Manage views first"),
        ),
        command(
            CommandId::Recipes,
            "Recipes",
            "Open saved recipe controls",
            "Recipes",
            &["saved view", "config"],
            Action::OpenRecipes,
            view_reason,
        ),
        command(
            CommandId::RecipeBrowse,
            "Browse recipes",
            "Select browse mode",
            "Recipes",
            &["list", "load"],
            Action::SelectRecipeMode(RecipeDialogMode::Browse),
            focus_reason(Focus::Recipes, "open Recipes first"),
        ),
        command(
            CommandId::RecipeSave,
            "Save recipe",
            "Select save mode",
            "Recipes",
            &["remember", "persist"],
            Action::SelectRecipeMode(RecipeDialogMode::Save),
            focus_reason(Focus::Recipes, "open Recipes first"),
        ),
        command(
            CommandId::RecipeImport,
            "Import recipe",
            "Select import mode",
            "Recipes",
            &["toml", "install"],
            Action::SelectRecipeMode(RecipeDialogMode::Import),
            focus_reason(Focus::Recipes, "open Recipes first"),
        ),
        command(
            CommandId::RecipeApply,
            "Apply selected recipe",
            "Apply the selected compatible recipe",
            "Recipes",
            &["load recipe", "use saved view"],
            Action::SubmitRecipe,
            (context.focus != Focus::Recipes
                || context.recipe_mode != Some(RecipeDialogMode::Browse))
            .then_some("open Recipes in browse mode first"),
        ),
        command(
            CommandId::RecipeRefreshSuggestions,
            "Refresh recipe suggestions",
            "Rank similar-source recipes from bounded evidence",
            "Recipes",
            &["similar source", "recommend recipe"],
            Action::RefreshRecipeSuggestions,
            focus_reason(Focus::Recipes, "open Recipes first"),
        ),
        command(
            CommandId::RecipeAdaptSuggestion,
            "Adapt suggested recipe with agent",
            "Review a fixed-snapshot typed filter adaptation",
            "Recipes",
            &["paseo", "suggestion", "adapt"],
            Action::AdaptRecipeSuggestion,
            focus_reason(Focus::Recipes, "open Recipes first"),
        ),
        command(
            CommandId::RecipeRejectSuggestion,
            "Reject selected recipe suggestion",
            "Record rejection without deleting the saved recipe",
            "Recipes",
            &["dismiss suggestion", "not relevant"],
            Action::RejectRecipeSuggestion,
            focus_reason(Focus::Recipes, "open Recipes first"),
        ),
        command(
            CommandId::Fields,
            "Fields",
            "Open field visibility and color controls",
            "Fields",
            &["columns", "schema"],
            Action::OpenFieldPicker,
            view_reason,
        ),
        command(
            CommandId::PinField,
            "Pin or unpin field",
            "Toggle the selected field",
            "Fields",
            &["column", "visible"],
            Action::TogglePinnedField,
            focus_reason(Focus::FieldPicker, "open Fields first"),
        ),
        command(
            CommandId::ColorField,
            "Color field",
            "Toggle color rules for the selected field",
            "Fields",
            &["highlight", "style"],
            Action::ToggleColorField,
            focus_reason(Focus::FieldPicker, "open Fields first"),
        ),
        command(
            CommandId::ScrollLogLeft,
            "Scroll log left",
            "Scroll event text while keeping metadata fixed",
            "View",
            &["horizontal", "pan"],
            Action::MoveHorizontal(-8),
            view_reason,
        ),
        command(
            CommandId::ScrollLogRight,
            "Scroll log right",
            "Scroll event text while keeping metadata fixed",
            "View",
            &["horizontal", "pan"],
            Action::MoveHorizontal(8),
            view_reason,
        ),
        command(
            CommandId::ResetLogHorizontal,
            "Reset horizontal scroll",
            "Scroll event text while keeping metadata fixed",
            "View",
            &["horizontal", "pan"],
            Action::ResetHorizontal,
            view_reason,
        ),
        command(
            CommandId::Follow,
            "Follow new records",
            "Toggle tail following",
            "View",
            &["tail", "live"],
            Action::ToggleFollow,
            view_reason,
        ),
        command(
            CommandId::Details,
            "Record details",
            "Toggle selected record details",
            "View",
            &["inspect", "row"],
            Action::ToggleDetails,
            view_reason,
        ),
        command(
            CommandId::StoragePreview,
            "Storage preview",
            "Inspect derived data before cleanup",
            "Storage",
            &["disk", "cache", "cleanup"],
            Action::OpenStorage,
            None,
        ),
        command(
            CommandId::StorageClear,
            "Confirm derived-data cleanup",
            "Use the existing two-step storage confirmation",
            "Storage",
            &["clear cache", "delete derived"],
            Action::ClearStorage,
            (context.focus != Focus::Storage || !context.storage_confirmation_ready)
                .then_some("confirm cleanup in Storage preview first"),
        ),
        command(
            CommandId::TimestampAssistant,
            "Recognize timestamp",
            "Prepare a reviewed UTC timestamp enrichment proposal",
            "Agent",
            &["date", "time", "format", "brain"],
            Action::OpenTimestampAssistant,
            view_reason,
        ),
        command(
            CommandId::AskAi,
            "Ask agent",
            "Open a typed filter or enrichment proposal",
            "agent",
            &["assistant", "proposal"],
            Action::OpenAskAi,
            view_reason,
        ),
        command(
            CommandId::AskAiFilter,
            "Ask agent for filter",
            "Select a filter proposal",
            "agent",
            &["predicate proposal"],
            Action::SelectAskAiKind(AskAiKind::Filter),
            focus_reason(Focus::AskAi, "open Ask agent first"),
        ),
        command(
            CommandId::AskAiEnrichment,
            "Ask agent for enrichment",
            "Select an enrichment proposal",
            "agent",
            &["derive proposal"],
            Action::SelectAskAiKind(AskAiKind::Enrichment),
            focus_reason(Focus::AskAi, "open Ask agent first"),
        ),
        command(
            CommandId::Investigations,
            "Investigations",
            "Open resumable investigation conversations",
            "agent",
            &["sessions", "analysis"],
            Action::OpenInvestigation,
            view_reason,
        ),
        command(
            CommandId::NewInvestigation,
            "New investigation",
            "Start a new investigation draft",
            "agent",
            &["question", "conversation"],
            Action::NewInvestigation,
            focus_reason(Focus::Investigation, "open Investigations first"),
        ),
        command(
            CommandId::ResumeInvestigation,
            "Resume selected investigation",
            "Resume the selected saved session",
            "agent",
            &["continue session", "history"],
            Action::SubmitInvestigation,
            (context.focus != Focus::Investigation || !context.investigation_can_resume)
                .then_some("open Investigations and select a saved session first"),
        ),
        command(
            CommandId::InvestigationFollowup,
            "Send investigation follow-up",
            "Send the current prompt to the active session",
            "agent",
            &["reply", "continue conversation"],
            Action::SubmitInvestigation,
            (context.focus != Focus::Investigation || !context.investigation_can_follow_up)
                .then_some("open an active investigation and enter a follow-up first"),
        ),
        command(
            CommandId::Settings,
            "Settings",
            "Edit global agent, appearance, and cache settings",
            "Application",
            &["preferences", "theme", "provider", "cache"],
            Action::OpenSettings,
            None,
        ),
        command(
            CommandId::Help,
            "Help",
            "Show keyboard help",
            "Application",
            &["keys", "shortcuts"],
            Action::ToggleHelp,
            None,
        ),
        command(
            CommandId::Quit,
            "Quit",
            "Exit after normal bounded shutdown",
            "Application",
            &["exit", "close"],
            Action::Quit,
            None,
        ),
    ];
    for command in &mut commands {
        command.shortcut = shortcut_for(&command.action, context.focus);
    }
    commands
}

fn command(
    id: CommandId,
    name: &'static str,
    description: &'static str,
    category: &'static str,
    aliases: &'static [&'static str],
    action: Action,
    unavailable_reason: Option<&'static str>,
) -> Command {
    Command {
        id,
        name,
        description,
        category,
        aliases,
        action,
        shortcut: None,
        unavailable_reason,
    }
}

const SHORTCUT_CANDIDATES: &[(KeyCode, KeyModifiers, &str)] = &[
    (KeyCode::Char('c'), KeyModifiers::CONTROL, "Ctrl-C"),
    (KeyCode::Char('q'), KeyModifiers::NONE, "q"),
    (KeyCode::Char('?'), KeyModifiers::NONE, "?"),
    (KeyCode::Char('/'), KeyModifiers::NONE, "/"),
    (KeyCode::Char('p'), KeyModifiers::NONE, "p"),
    (KeyCode::Char('e'), KeyModifiers::NONE, "e"),
    (KeyCode::Char('m'), KeyModifiers::NONE, "m"),
    (KeyCode::Char('S'), KeyModifiers::SHIFT, "S"),
    (KeyCode::Char('A'), KeyModifiers::SHIFT, "A"),
    (KeyCode::Char('I'), KeyModifiers::SHIFT, "I"),
    (KeyCode::Char('n'), KeyModifiers::NONE, "n"),
    (KeyCode::Char('r'), KeyModifiers::NONE, "r"),
    (KeyCode::Char('t'), KeyModifiers::NONE, "t"),
    (KeyCode::Char('i'), KeyModifiers::NONE, "i"),
    (KeyCode::Char('v'), KeyModifiers::NONE, "v"),
    (KeyCode::Char(']'), KeyModifiers::NONE, "]"),
    (KeyCode::Char('['), KeyModifiers::NONE, "["),
    (KeyCode::Char('f'), KeyModifiers::NONE, "f"),
    (KeyCode::Left, KeyModifiers::NONE, "Left"),
    (KeyCode::Right, KeyModifiers::NONE, "Right"),
    (KeyCode::Char('0'), KeyModifiers::NONE, "0"),
    (KeyCode::Char('d'), KeyModifiers::NONE, "d"),
    (KeyCode::Char('c'), KeyModifiers::NONE, "c"),
    (KeyCode::Char('x'), KeyModifiers::NONE, "x"),
    (KeyCode::Char(' '), KeyModifiers::NONE, "Space"),
    (KeyCode::Enter, KeyModifiers::NONE, "Enter"),
    (KeyCode::Char('d'), KeyModifiers::CONTROL, "Ctrl-D"),
    (KeyCode::Char('a'), KeyModifiers::CONTROL, "Ctrl-A"),
    (KeyCode::Char('t'), KeyModifiers::ALT, "Alt-T"),
    (KeyCode::Char('u'), KeyModifiers::ALT, "Alt-U"),
    (KeyCode::Char('f'), KeyModifiers::ALT, "Alt-F"),
    (KeyCode::Char('e'), KeyModifiers::ALT, "Alt-E"),
    (KeyCode::Char('p'), KeyModifiers::ALT, "Alt-P"),
    (KeyCode::Char('a'), KeyModifiers::ALT, "Alt-A"),
    (KeyCode::Char('g'), KeyModifiers::ALT, "Alt-G"),
    (KeyCode::Char('c'), KeyModifiers::ALT, "Alt-C"),
    (KeyCode::Char('5'), KeyModifiers::ALT, "Alt-5"),
    (KeyCode::Char('m'), KeyModifiers::ALT, "Alt-M"),
    (KeyCode::Char('h'), KeyModifiers::ALT, "Alt-H"),
    (KeyCode::Char('b'), KeyModifiers::ALT, "Alt-B"),
    (KeyCode::Char('d'), KeyModifiers::ALT, "Alt-D"),
    (KeyCode::Char('r'), KeyModifiers::ALT, "Alt-R"),
    (KeyCode::Char('s'), KeyModifiers::ALT, "Alt-S"),
    (KeyCode::Char('i'), KeyModifiers::ALT, "Alt-I"),
    (KeyCode::Char('n'), KeyModifiers::ALT, "Alt-N"),
];

fn shortcut_for(action: &Action, focus: Focus) -> Option<&'static str> {
    SHORTCUT_CANDIDATES
        .iter()
        .find_map(|(code, modifiers, label)| {
            let key = KeyEvent::new(*code, *modifiers);
            (&key_to_action(key, focus) == action).then_some(*label)
        })
}
