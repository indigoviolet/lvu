//! Bounded command catalog and palette state. The terminal integration owns the
//! global toggle and passes a snapshot of application availability to `open`.

use crate::app::{Action, Focus, RecipeDialogMode, key_to_action};
use crate::component::{CommandEntry, LayerId};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

use crate::dialog_controls::DialogStyles;
use crate::text_edit::{
    EditCommand, EditPolicy, TextCursor, cursor_line_prefix, edit, reset_cursor_to_end,
};
use crate::theme::Theme;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub const MAX_QUERY_BYTES: usize = 256;
pub const MAX_RESULTS: usize = 128;

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
    CommandEnrichment,
    CommandEnrichmentSave,
    CommandEnrichmentRemove,
    CommandEnrichmentRun,
    EditorCompletion,
    Grouping,
    ToggleExpandedGroup,
    FoldingDialog,
    ToggleFolding,
    CollapseAllFolds,
    NextView,
    PreviousView,
    TimeWindow,
    TimeClear,
    TimeAroundSelected,
    StopCapture,
    RestartCapture,
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
    ViewSources,
    Recipes,
    RecipeBrowse,
    RecipeSave,
    RecipeImport,
    RecipeExport,
    RecipeHistory,
    RecipeUpdate,
    RecipeApply,
    RecipeRefreshSuggestions,
    RecipeAdaptSuggestion,
    RecipeRejectSuggestion,
    Fields,
    PinField,
    ColorField,
    CorrelateField,
    FilterToFieldValue,
    ExcludeFieldValue,
    FoldByField,
    DetailsExpand,
    ScrollLogLeft,
    ScrollLogRight,
    ResetLogHorizontal,
    JumpToFirst,
    JumpToLast,
    NextGap,
    PreviousGap,
    Follow,
    Details,
    DetailsScrollUp,
    DetailsScrollDown,
    DetailsTop,
    Context,
    ReturnFromRawContext,
    ToggleBookmark,
    Bookmarks,
    BookmarkNote,
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
    CommandId::CommandEnrichment,
    CommandId::CommandEnrichmentSave,
    CommandId::CommandEnrichmentRemove,
    CommandId::CommandEnrichmentRun,
    CommandId::EditorCompletion,
    CommandId::Grouping,
    CommandId::ToggleExpandedGroup,
    CommandId::FoldingDialog,
    CommandId::ToggleFolding,
    CommandId::CollapseAllFolds,
    CommandId::NextView,
    CommandId::PreviousView,
    CommandId::TimeWindow,
    CommandId::TimeClear,
    CommandId::TimeAroundSelected,
    CommandId::StopCapture,
    CommandId::RestartCapture,
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
    CommandId::ViewSources,
    CommandId::Recipes,
    CommandId::RecipeBrowse,
    CommandId::RecipeSave,
    CommandId::RecipeImport,
    CommandId::RecipeExport,
    CommandId::RecipeHistory,
    CommandId::RecipeUpdate,
    CommandId::RecipeApply,
    CommandId::RecipeRefreshSuggestions,
    CommandId::RecipeAdaptSuggestion,
    CommandId::RecipeRejectSuggestion,
    CommandId::Fields,
    CommandId::PinField,
    CommandId::ColorField,
    CommandId::CorrelateField,
    CommandId::ScrollLogLeft,
    CommandId::ScrollLogRight,
    CommandId::ResetLogHorizontal,
    CommandId::Follow,
    CommandId::Details,
    CommandId::DetailsScrollUp,
    CommandId::DetailsScrollDown,
    CommandId::DetailsTop,
    CommandId::Context,
    CommandId::ReturnFromRawContext,
    CommandId::ToggleBookmark,
    CommandId::Bookmarks,
    CommandId::BookmarkNote,
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
    CommandId::FilterToFieldValue,
    CommandId::ExcludeFieldValue,
    CommandId::FoldByField,
    CommandId::DetailsExpand,
    CommandId::JumpToFirst,
    CommandId::JumpToLast,
    CommandId::NextGap,
    CommandId::PreviousGap,
    CommandId::Help,
    CommandId::Quit,
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PaletteContext {
    pub focus: Focus,
    pub has_view: bool,
    pub has_selected_row: bool,
    /// `o` has an origin and the raw view it landed in is active, so `o`
    /// returns (docs/raw-context-as-jump.md).
    pub raw_context_held: bool,
    /// The active view is its source's All events view: there is no raw
    /// stream to jump to.
    pub in_raw_view: bool,
    /// Entries the converted components declare for themselves (§4.3). The
    /// palette no longer inspects dialog state to decide availability.
    pub layer_commands: Vec<(LayerId, CommandEntry)>,
}

impl PaletteContext {
    pub fn new(focus: Focus, has_view: bool) -> Self {
        Self {
            focus,
            has_view,
            has_selected_row: false,
            raw_context_held: false,
            in_raw_view: false,
            layer_commands: Vec::new(),
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
    selection_area: Option<Rect>,
    open: bool,
    return_focus: Focus,
    context: PaletteContext,
    query: String,
    query_cursor: TextCursor,
    commands: Vec<Command>,
    matches: Vec<usize>,
    /// Where the `Not available now` group starts in `matches` (§8.10): the
    /// commands a query matched that cannot run in the current focus and
    /// state, listed after every one that can, each with its reason. `None`
    /// when nothing unavailable matched, and always for a blank query, whose
    /// list holds only what can run.
    unavailable_start: Option<usize>,
    selected: usize,
    /// Scroll offset in *display* rows, which include the group heading.
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
            selection_area: None,
            open: false,
            return_focus: Focus::Logs,
            context,
            query: String::new(),
            query_cursor: TextCursor::default(),
            commands: Vec::new(),
            matches: Vec::new(),
            unavailable_start: None,
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
        self.query_cursor = TextCursor::default();
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

    pub fn context(&self) -> &PaletteContext {
        &self.context
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

    /// The matched commands that cannot run now, in list order. Empty for a
    /// blank query: the default list is only what can run (§8.10).
    pub fn unavailable_results(&self) -> impl Iterator<Item = &Command> {
        let start = self.unavailable_start.unwrap_or(self.matches.len());
        self.matches[start..]
            .iter()
            .map(|index| &self.commands[*index])
    }

    /// Display row of a match: matches after the group heading sit one row
    /// lower than their index.
    fn display_index(&self, result: usize) -> usize {
        match self.unavailable_start {
            Some(start) if result >= start => result + 1,
            _ => result,
        }
    }

    fn display_rows(&self) -> usize {
        self.matches.len() + usize::from(self.unavailable_start.is_some())
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
            KeyCode::Left => {
                self.edit_query(EditCommand::MoveLeft);
            }
            KeyCode::Right => {
                self.edit_query(EditCommand::MoveRight);
            }
            KeyCode::Backspace => {
                if self.edit_query(EditCommand::Backspace) {
                    self.refresh_matches();
                }
            }
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.edit_query(EditCommand::StartOfLine);
            }
            KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.edit_query(EditCommand::EndOfLine);
            }
            KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.edit_query(EditCommand::KillToEndOfLine) {
                    self.refresh_matches();
                }
            }
            KeyCode::Tab => {
                if let Some(name) = self.selected_command().map(|command| command.name) {
                    self.query.clear();
                    self.query.push_str(name);
                    reset_cursor_to_end(&self.query, &mut self.query_cursor);
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
                self.insert_query(&character.to_string());
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
            self.insert_query(text);
        }
    }

    pub fn selection_area(&self) -> Option<Rect> {
        self.selection_area.filter(|_| self.open)
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
        self.selection_area = None;
        if !self.open || area.width < 4 || area.height < 3 {
            return;
        }
        // §12.16 class P: transient, top-anchored, list-driven. Sharing the
        // class table is what keeps the palette proportioned like every other
        // surface instead of stretching a name column across the frame.
        let class = crate::dialog_layout::DialogClass::P;
        let width = class.width(area);
        let height = class.max_height(area).min(area.height);
        let popup = Rect::new(
            area.x + area.width.saturating_sub(width) / 2,
            area.y
                + (area.height.saturating_sub(height) / 6).min(area.height.saturating_sub(height)),
            width,
            height,
        );
        frame.render_widget(Clear, popup);
        let block = Block::default()
            .title(" Command palette ")
            .borders(Borders::ALL)
            .style(Style::default().fg(theme.base_fg).bg(theme.dialog_bg))
            .border_style(Style::default().fg(theme.active_border));
        let inner = block.inner(popup);
        self.selection_area = Some(inner);
        frame.render_widget(block, popup);
        if inner.height == 0 {
            return;
        }
        let selected = self.selected_command();
        let detail_height = palette_detail_height(inner, selected);
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(0),
                Constraint::Length(detail_height),
            ])
            .split(inner);
        let styles = DialogStyles::new(theme);
        let input = Rect::new(
            chunks[0].x.saturating_add(2),
            chunks[0].y,
            chunks[0].width.saturating_sub(2),
            1,
        );
        let (visible_query, column) = palette_input_window(
            &self.query,
            &mut self.query_cursor,
            usize::from(input.width),
        );
        if self.query.is_empty() {
            // §8.1: the field keeps its input tone whether or not it has text,
            // so the placeholder is muted *on* the surface rather than instead
            // of it.
            frame.render_widget(
                Paragraph::new("Type a command…")
                    .style(styles.input.patch(styles.unavailable.bg(theme.input_bg))),
                input,
            );
        } else {
            frame.render_widget(Paragraph::new(visible_query).style(styles.input), input);
        }
        if chunks[0].width > 2 {
            let x = input.x + column as u16;
            frame.buffer_mut()[(x, chunks[0].y)]
                .set_style(Style::default().fg(theme.input_fg).bg(theme.cursor));
            frame.set_cursor_position((x, chunks[0].y));
        }
        self.visible_rows = chunks[1].height as usize;
        self.keep_selected_visible();
        // Display rows: every match, plus one heading row before the
        // `Not available now` group when a query matched something that
        // cannot run here (§8.10). The heading is not selectable.
        let display: Vec<Option<usize>> = (0..self.matches.len())
            .flat_map(|result| {
                let heading = (Some(result) == self.unavailable_start).then_some(None);
                heading.into_iter().chain(std::iter::once(Some(result)))
            })
            .collect();
        let end = (self.scroll + self.visible_rows).min(display.len());
        let visible_rows = &display[self.scroll.min(end)..end];
        let visible: Vec<usize> = visible_rows
            .iter()
            .filter_map(|row| row.map(|result| self.matches[result]))
            .collect();
        // §9: a list longer than its viewport says so in the last column,
        // rather than leaving the user to discover it by pressing Down.
        let overflowing = display.len() > self.visible_rows;
        let list = Rect::new(
            chunks[1].x,
            chunks[1].y,
            chunks[1].width.saturating_sub(u16::from(overflowing)),
            chunks[1].height,
        );
        let name_width = visible
            .iter()
            .map(|index| UnicodeWidthStr::width(self.commands[*index].name))
            .max()
            .unwrap_or(0)
            .min(32);
        let shortcut_width = visible
            .iter()
            .filter_map(|index| self.commands[*index].shortcut)
            .map(UnicodeWidthStr::width)
            .max()
            .unwrap_or(0)
            .min(14);
        let columns = palette_columns(list, name_width, shortcut_width);
        for (screen_row, entry) in visible_rows.iter().enumerate() {
            let row = Rect::new(list.x, list.y + screen_row as u16, list.width, 1);
            let Some(result_index) = *entry else {
                // The group heading: a §8.7 pane heading, not a row.
                frame.render_widget(
                    Block::default().style(styles.label.bg(theme.dialog_bg)),
                    row,
                );
                render_palette_cell(
                    frame,
                    columns.name_at(row),
                    UNAVAILABLE_HEADING,
                    styles
                        .label
                        .bg(theme.dialog_bg)
                        .add_modifier(Modifier::BOLD),
                );
                continue;
            };
            let command = &self.commands[self.matches[result_index]];
            let selected = result_index == self.selected;
            let prefix = if selected { "› " } else { "  " };
            let row_style = if selected {
                styles.selection
            } else if command.is_enabled() {
                styles.label.bg(theme.dialog_bg)
            } else {
                styles
                    .unavailable
                    .bg(theme.dialog_bg)
                    .add_modifier(Modifier::ITALIC)
            };
            let shortcut_style = if selected {
                styles.selection
            } else if command.is_enabled() {
                styles.shortcut.bg(theme.dialog_bg)
            } else {
                styles.unavailable.bg(theme.dialog_bg)
            };
            frame.render_widget(Block::default().style(row_style), row);
            render_palette_cell(frame, columns.prefix_at(row), prefix, row_style);
            render_palette_cell(frame, columns.name_at(row), command.name, row_style);
            match command.unavailable_reason {
                // An unavailable row carries its reason where the chord and
                // category would be: neither applies to a command that
                // cannot run, and the reason is what the reader needs.
                Some(reason) => {
                    let start = columns.shortcut_at(row);
                    let span = Rect::new(start.x, row.y, row.right().saturating_sub(start.x), 1);
                    // §9: a reason the row cannot hold whole is cut with an
                    // ellipsis; the detail row carries it whole when selected.
                    let text = crate::ui::truncated(reason, usize::from(span.width));
                    render_palette_cell(
                        frame,
                        span,
                        &text,
                        if selected {
                            styles.selection
                        } else {
                            styles.unavailable.bg(theme.dialog_bg)
                        },
                    );
                }
                None => {
                    let shortcut = command.shortcut.unwrap_or("");
                    render_palette_cell(frame, columns.shortcut_at(row), shortcut, shortcut_style);
                    render_palette_cell(
                        frame,
                        columns.category_at(row),
                        command.category,
                        row_style,
                    );
                }
            }
            self.rows.push((row, result_index));
        }
        if visible.is_empty() {
            frame.render_widget(
                Paragraph::new("  No matching commands").style(styles.description),
                chunks[1],
            );
        }
        if overflowing {
            crate::ui::render_scrollbar(
                frame,
                Rect::new(
                    chunks[1].right().saturating_sub(1),
                    chunks[1].y,
                    1,
                    chunks[1].height,
                ),
                self.scroll,
                self.display_rows().saturating_sub(self.visible_rows),
                theme,
                // The palette is drawn by the shell, which owns the ASCII
                // choice; the scrollbar follows the theme's glyph set.
                false,
            );
        }
        if detail_height > 0
            && let Some(command) = self.selected_command()
        {
            // One row, one sentence: what the selected command does, or why it
            // cannot be run. The name is not repeated — its row is right above,
            // marked.
            let lines: Vec<Line<'_>> = palette_detail_lines(command, inner.width.saturating_sub(2))
                .into_iter()
                .map(|part| match part {
                    DetailPart::Name(name) => Line::styled(name, styles.label),
                    DetailPart::NameAnd(name, rest) => Line::from(vec![
                        Span::styled(name, styles.label),
                        Span::styled(" · ", styles.description),
                        Span::styled(rest, styles.description),
                    ]),
                    DetailPart::Description(text) => Line::styled(text, styles.description),
                    DetailPart::Unavailable(text) => Line::from(vec![
                        Span::styled(UNAVAILABLE_LABEL, styles.error),
                        Span::styled(text, styles.unavailable),
                    ]),
                    DetailPart::Clause(text) => Line::styled(text, styles.unavailable),
                })
                .collect();
            // §4.1: the row lines up with the list above it.
            let detail = Rect::new(
                chunks[2].x.saturating_add(2),
                chunks[2].y,
                chunks[2].width.saturating_sub(2),
                chunks[2].height,
            );
            frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), detail);
        }
    }

    fn edit_query(&mut self, command: EditCommand<'_>) -> bool {
        edit(
            &mut self.query,
            &mut self.query_cursor,
            command,
            EditPolicy {
                max_bytes: MAX_QUERY_BYTES,
                multiline: false,
            },
        )
        .changed
    }

    fn insert_query(&mut self, text: &str) {
        if self.edit_query(EditCommand::Insert(text)) {
            self.refresh_matches();
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
        let mut row = self.display_index(self.selected);
        // The group heading belongs with the first row under it.
        if Some(self.selected) == self.unavailable_start {
            row = row.saturating_sub(1);
        }
        if row < self.scroll {
            self.scroll = row;
        } else if self.display_index(self.selected) >= self.scroll + visible {
            self.scroll = self.display_index(self.selected) + 1 - visible;
        }
        self.scroll = self.scroll.min(self.display_rows().saturating_sub(visible));
    }

    fn replace_catalog(&mut self) {
        self.commands = catalog(&self.context);
        self.refresh_matches();
    }

    fn refresh_matches(&mut self) {
        let blank = self.query.trim().is_empty();
        let mut scored: Vec<(usize, u32)> = self
            .commands
            .iter()
            .enumerate()
            .filter(|(_, command)| !blank || command.is_enabled())
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
        // §8.10: what can run first, in score order; what cannot, after it as
        // its own group, in score order, each with the reason the control
        // that owns it gives. A stable partition keeps both orders.
        let (available, unavailable): (Vec<_>, Vec<_>) = scored
            .into_iter()
            .partition(|(index, _)| self.commands[*index].is_enabled());
        self.unavailable_start = (!unavailable.is_empty()).then_some(available.len());
        self.matches = available
            .into_iter()
            .chain(unavailable)
            .map(|(index, _)| index)
            .collect();
        self.selected = self.selected.min(self.matches.len().saturating_sub(1));
        self.scroll = 0;
    }
}

#[derive(Clone, Copy)]
struct PaletteColumns {
    name_x: u16,
    name_width: u16,
    shortcut_x: u16,
    shortcut_width: u16,
    category_x: u16,
    category_width: u16,
}

impl PaletteColumns {
    fn prefix_at(self, row: Rect) -> Rect {
        Rect::new(row.x, row.y, row.width.min(2), 1)
    }

    fn name_at(self, row: Rect) -> Rect {
        Rect::new(self.name_x, row.y, self.name_width, 1)
    }

    fn shortcut_at(self, row: Rect) -> Rect {
        Rect::new(self.shortcut_x, row.y, self.shortcut_width, 1)
    }

    fn category_at(self, row: Rect) -> Rect {
        Rect::new(self.category_x, row.y, self.category_width, 1)
    }
}

/// §12.16 columns: the name fills, the shortcut and the category are fixed and
/// sit against the right edge. Fixed trailing columns keep the two right-hand
/// columns aligned whatever the names do, which is what makes the list
/// scannable — a name-driven category column drifts with every query.
const PALETTE_SHORTCUT_WIDTH: u16 = 8;
const PALETTE_CATEGORY_WIDTH: u16 = 12;

fn palette_columns(area: Rect, desired_name: usize, desired_shortcut: usize) -> PaletteColumns {
    let prefix_width = area.width.min(2);
    let available = area.width.saturating_sub(prefix_width);
    let shortcut = u16::try_from(desired_shortcut)
        .unwrap_or(u16::MAX)
        .min(PALETTE_SHORTCUT_WIDTH);
    // The trailing columns only earn their place when a readable name still
    // fits beside them.
    let show_shortcut = shortcut > 0 && available >= shortcut.saturating_add(8);
    let shortcut_width = if show_shortcut { shortcut } else { 0 };
    let category_width = PALETTE_CATEGORY_WIDTH.min(
        available
            .saturating_sub(shortcut_width)
            .saturating_sub(12)
            .min(PALETTE_CATEGORY_WIDTH),
    );
    let name_x = area.x.saturating_add(prefix_width);
    let category_x = area.right().saturating_sub(category_width);
    let shortcut_x = category_x
        .saturating_sub(u16::from(category_width > 0) * 2)
        .saturating_sub(shortcut_width);
    let name_width = shortcut_x
        .saturating_sub(u16::from(shortcut_width > 0) * 2)
        .saturating_sub(name_x)
        .min(u16::try_from(desired_name).unwrap_or(u16::MAX).max(1));
    PaletteColumns {
        name_x,
        name_width,
        shortcut_x,
        shortcut_width,
        category_x,
        category_width,
    }
}

fn render_palette_cell(frame: &mut Frame<'_>, area: Rect, text: &str, style: Style) {
    if area.is_empty() {
        return;
    }
    frame.render_widget(
        Paragraph::new(palette_column_text(text, usize::from(area.width))).style(style),
        area,
    );
}

fn palette_column_text(value: &str, maximum_width: usize) -> String {
    let mut output = String::new();
    let mut width = 0usize;
    for character in value.chars() {
        let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if output.is_empty() && character_width == 0 {
            continue;
        }
        if width.saturating_add(character_width) > maximum_width {
            break;
        }
        output.push(character);
        width = width.saturating_add(character_width);
    }
    output
}

fn palette_detail_height(area: Rect, command: Option<&Command>) -> u16 {
    if area.height < 3 || area.width == 0 || command.is_none() {
        return 0;
    }
    let command = command.expect("checked above");
    let available = area.height.saturating_sub(2);
    let width = area.width.saturating_sub(2);
    let lines = palette_detail_lines(command, width).len();
    u16::try_from(lines).unwrap_or(u16::MAX).min(available)
}

/// One piece of the detail row. §12.16 puts the selected command's name and
/// what it does on the message row; a clipped name in the list is therefore
/// always readable in full here, which is the guarantee the old
/// `Selected: <name>` echo carried before §7.4 retired that stutter.
enum DetailPart<'a> {
    Name(&'a str),
    NameAnd(&'a str, &'a str),
    Description(&'a str),
    Unavailable(&'a str),
    Clause(&'a str),
}

/// The detail row's lines. Shared by measuring and drawing so the rows reserved
/// are the rows written.
fn palette_detail_lines<'a>(command: &'a Command, width: u16) -> Vec<DetailPart<'a>> {
    let width = usize::from(width.max(1));
    let fits =
        |a: &str, b: &str| UnicodeWidthStr::width(a) + 3 + UnicodeWidthStr::width(b) <= width;
    let Some(reason) = command.unavailable_reason else {
        if fits(command.name, command.description) {
            return vec![DetailPart::NameAnd(command.name, command.description)];
        }
        return vec![
            DetailPart::Name(command.name),
            DetailPart::Description(command.description),
        ];
    };
    // A reason with two clauses reads as one line per clause rather than as a
    // paragraph wrapped mid-clause: the clause is the unit a reader needs whole.
    let mut clauses = reason.split(" and ");
    let first = clauses.next().unwrap_or(reason);
    let mut lines = vec![DetailPart::Name(command.name)];
    if UnicodeWidthStr::width(UNAVAILABLE_LABEL) + UnicodeWidthStr::width(first) <= width {
        lines.push(DetailPart::Unavailable(first));
    } else {
        lines.push(DetailPart::Unavailable(""));
        lines.push(DetailPart::Clause(first));
    }
    lines.extend(clauses.map(DetailPart::Clause));
    lines
}

/// §12.16 puts the reason on the message row. A reason with two clauses reads
/// as one line per clause rather than as a paragraph wrapped mid-clause, and
/// the label joins the first clause only when it does not push it onto a second
/// line — a clause split across rows is the thing a reader has to reassemble.
const UNAVAILABLE_LABEL: &str = "Unavailable: ";

/// The heading over the matched commands that cannot run now (§8.10).
pub const UNAVAILABLE_HEADING: &str = "Not available now";

fn palette_input_window(
    value: &str,
    cursor: &mut TextCursor,
    maximum_width: usize,
) -> (String, usize) {
    if maximum_width == 0 {
        return (String::new(), 0);
    }
    cursor_line_prefix(value, cursor);
    let at = value
        .char_indices()
        .nth(cursor.char_index)
        .map_or(value.len(), |(index, _)| index);
    let visible_before = palette_input_tail(&value[..at], maximum_width.saturating_sub(1));
    let cursor_column = UnicodeWidthStr::width(visible_before.as_str());
    let mut visible = visible_before;
    let mut width = cursor_column;
    for character in value[at..].chars() {
        let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if visible.is_empty() && character_width == 0 {
            continue;
        }
        if width.saturating_add(character_width) > maximum_width {
            break;
        }
        visible.push(character);
        width = width.saturating_add(character_width);
    }
    (visible, cursor_column)
}

fn palette_input_tail(value: &str, maximum_width: usize) -> String {
    let mut width = 0usize;
    let mut start = value.len();
    for (index, character) in value.char_indices().rev() {
        let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if width.saturating_add(character_width) > maximum_width {
            break;
        }
        width = width.saturating_add(character_width);
        start = index;
    }
    while let Some(character) = value[start..].chars().next() {
        if UnicodeWidthChar::width(character).unwrap_or(0) != 0 {
            break;
        }
        start = start.saturating_add(character.len_utf8());
    }
    value[start..].to_owned()
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

fn catalog(context: &PaletteContext) -> Vec<Command> {
    let view_reason = (!context.has_view).then_some("open a source first");
    let focus_reason =
        |focus: Focus, reason: &'static str| (context.focus != focus).then_some(reason);
    let mut commands = vec![
        command(
            CommandId::AddSource,
            "Add source",
            "Open the admitted source dialog",
            "Sources",
            &["new source", "file", "command"],
            Action::Open(crate::component::Open::Source),
            None,
        ),
        command(
            CommandId::LiteralFilter,
            "Literal filter",
            "Edit case-insensitive text search",
            "Filter",
            &["search", "grep", "text"],
            Action::Open(crate::component::Open::Search),
            view_reason,
        ),
        command(
            CommandId::AdvancedFilter,
            "Advanced filter",
            "Edit the Polars predicate",
            "Filter",
            &["predicate", "where", "polars"],
            Action::Open(crate::component::Open::Advanced),
            view_reason,
        ),
        command(
            CommandId::Enrichment,
            "Enrichment",
            "Open ordered derived-field stages",
            "Filter",
            &["derive", "column", "polars"],
            Action::Open(crate::component::Open::Enrichment),
            view_reason,
        ),
        command(
            CommandId::CommandEnrichment,
            "Terminal command step",
            "Add or edit one explicitly run command after enrichment steps",
            "Enrichment",
            &["executable", "structured command", "external fields"],
            Action::Open(crate::component::Open::ExternalCommand {
                stage: None,
                insert_at: usize::MAX,
            }),
            view_reason,
        ),
        command(
            CommandId::EditorCompletion,
            "Complete editor field or value",
            "Insert a sampled field expression or lexical string without applying",
            "Filter",
            &["autocomplete", "field picker", "sampled value"],
            Action::None,
            // Both owners of this command are layers now (Advanced and the
            // enrichment step editor); whichever is open takes the row over.
            Some("open Advanced filter or Enrichment first"),
        ),
        command(
            CommandId::Grouping,
            "Grouping",
            "Edit continuation grouping",
            "Filter",
            &["multiline", "group"],
            Action::Open(crate::component::Open::Grouping),
            view_reason,
        ),
        command(
            CommandId::ToggleExpandedGroup,
            "Expand or collapse group",
            "Toggle the selected continuation group",
            "Views",
            &["stack trace", "multiline", "fold"],
            Action::ToggleExpandedGroup,
            (!context.has_selected_row).then_some("select a grouped row first"),
        ),
        command(
            CommandId::FoldingDialog,
            "Folding",
            "Choose the column runs fold on, the minimum run and the scope",
            "Views",
            &["fold", "repeat", "collapse", "dedupe", "pattern", "column"],
            Action::Open(crate::component::Open::Folding),
            view_reason,
        ),
        command(
            CommandId::ToggleFolding,
            "Fold repeated events",
            "Collapse runs of near-identical events into one counted line",
            "Views",
            &["repeat", "flood", "collapse", "dedupe", "noise"],
            Action::ToggleFolding,
            view_reason,
        ),
        command(
            CommandId::CollapseAllFolds,
            "Collapse expanded runs",
            "Return every expanded repeated run to its counted line",
            "Views",
            &["fold", "repeat", "collapse all"],
            Action::CollapseAllFolds,
            view_reason,
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
        // §8.10: every base-screen operation is in the index, including the
        // navigation that has no control of its own.
        command(
            CommandId::JumpToFirst,
            "Jump to first record",
            "Select the first record in the view",
            "Views",
            &["top", "start"],
            Action::Top,
            view_reason,
        ),
        command(
            CommandId::JumpToLast,
            "Jump to last record",
            "Select the last record in the view",
            "Views",
            &["bottom", "end"],
            Action::End,
            view_reason,
        ),
        command(
            CommandId::NextGap,
            "Next quiet period",
            "Jump forward past the next gap longer than the Time window's gap threshold",
            "Views",
            &["gap", "silence"],
            Action::JumpToGap(crate::provider::GapDirection::Forward),
            view_reason,
        ),
        command(
            CommandId::PreviousGap,
            "Previous quiet period",
            "Jump back past the previous gap longer than the Time window's gap threshold",
            "Views",
            &["gap", "silence"],
            Action::JumpToGap(crate::provider::GapDirection::Backward),
            view_reason,
        ),
        command(
            CommandId::TimeWindow,
            "Time window",
            "Open time range and basis controls",
            "Time",
            &["range", "timestamp"],
            Action::Open(crate::component::Open::Time),
            view_reason,
        ),
        command(
            CommandId::StopCapture,
            "Stop source capture",
            "Gracefully stop the shared source; keep journal and views",
            "Sources",
            &["stop", "capture", "process"],
            Action::StopCapture,
            (!context.has_view)
                .then_some("select a source view first")
                .or((!matches!(context.focus, Focus::Logs | Focus::Selector))
                    .then_some("close the current dialog first")),
        ),
        command(
            CommandId::RestartCapture,
            "Restart source capture",
            "Stop then restart the shared file or command source",
            "Sources",
            &["restart", "capture", "process"],
            Action::RestartCapture,
            (!context.has_view)
                .then_some("select a source view first")
                .or((!matches!(context.focus, Focus::Logs | Focus::Selector))
                    .then_some("close the current dialog first")),
        ),
        command(
            CommandId::ViewDialog,
            "Manage views",
            "Open blank, clone, and rename choices",
            "Views",
            &["new view", "copy view"],
            Action::Open(crate::component::Open::View),
            view_reason,
        ),
        command(
            CommandId::Recipes,
            "Recipes",
            "Open saved recipe controls",
            "Recipes",
            &["saved views", "presets"],
            Action::Open(crate::component::Open::Recipes {
                mode: RecipeDialogMode::Browse,
            }),
            None,
        ),
        command(
            CommandId::Fields,
            "Fields",
            "Open field visibility and color controls",
            "Fields",
            &["columns", "schema"],
            Action::Open(crate::component::Open::Fields),
            view_reason,
        ),
        command(
            CommandId::ScrollLogLeft,
            "Scroll log left",
            "Scroll event text while keeping metadata fixed",
            "Views",
            &["horizontal", "pan"],
            Action::MoveHorizontal(-8),
            view_reason,
        ),
        command(
            CommandId::ScrollLogRight,
            "Scroll log right",
            "Scroll event text while keeping metadata fixed",
            "Views",
            &["horizontal", "pan"],
            Action::MoveHorizontal(8),
            view_reason,
        ),
        command(
            CommandId::ResetLogHorizontal,
            "Reset horizontal scroll",
            "Scroll event text while keeping metadata fixed",
            "Views",
            &["horizontal", "pan"],
            Action::ResetHorizontal,
            view_reason,
        ),
        command(
            CommandId::Follow,
            "Follow new records",
            "Toggle tail following",
            "Views",
            &["tail", "live"],
            Action::ToggleFollow,
            view_reason,
        ),
        command(
            CommandId::Details,
            "Record details",
            "Toggle selected record details",
            "Views",
            &["inspect", "row"],
            Action::ToggleDetails,
            view_reason,
        ),
        command(
            CommandId::DetailsScrollUp,
            "Scroll details up",
            "Inspect earlier wrapped fields without changing the selected record",
            "Views",
            &["details scroll up", "previous field"],
            Action::ScrollDetails(-6),
            view_reason,
        ),
        command(
            CommandId::DetailsScrollDown,
            "Scroll details down",
            "Inspect later wrapped fields without changing the selected record",
            "Views",
            &["details scroll down", "next field", "command result"],
            Action::ScrollDetails(6),
            view_reason,
        ),
        command(
            CommandId::DetailsExpand,
            "Expand or collapse value",
            "Open or close the object or array under the Details cursor",
            "Views",
            &["nested", "json", "tree"],
            Action::DetailsPath(None),
            focus_reason(Focus::Details, "focus the details pane first"),
        ),
        command(
            CommandId::DetailsTop,
            "Reset details scroll",
            "Return Details to the selected record identity and raw value",
            "Views",
            &["details top", "details home"],
            Action::ResetDetails,
            view_reason,
        ),
        command(
            CommandId::Context,
            "Raw context",
            "Jump to the selected record in its source's All events view; o again returns",
            "Views",
            &["neighbors", "surrounding", "unfiltered", "all events"],
            Action::RawContext {
                anchor: None,
                layer: None,
            },
            if !context.has_view || !matches!(context.focus, Focus::Logs | Focus::Selector) {
                Some("select a log record first")
            } else if context.raw_context_held {
                Some("return with o first")
            } else if context.in_raw_view {
                Some("this is the raw stream")
            } else if !context.has_selected_row {
                Some("no record selected")
            } else {
                None
            },
        ),
        command(
            CommandId::ReturnFromRawContext,
            "Back from raw context",
            "Return to the view, record and dialog o was pressed in",
            "Views",
            &["return", "back", "filtered view"],
            Action::ReturnFromRawContext,
            if context.raw_context_held && matches!(context.focus, Focus::Logs | Focus::Selector) {
                None
            } else {
                Some("nothing to return to")
            },
        ),
        command(
            CommandId::ToggleBookmark,
            "Toggle record bookmark",
            "Bookmark the selected stable record",
            "Views",
            &["mark", "remember"],
            Action::ToggleBookmark,
            if context.has_view && matches!(context.focus, Focus::Logs | Focus::Selector) {
                None
            } else {
                Some("select a log record first")
            },
        ),
        command(
            CommandId::Bookmarks,
            "Bookmarks and notes",
            "Browse saved record bookmarks",
            "Views",
            &["annotations", "marks"],
            Action::Open(crate::component::Open::Bookmarks),
            if context.has_view && matches!(context.focus, Focus::Logs | Focus::Selector) {
                None
            } else {
                Some("open a log view first")
            },
        ),
        command(
            CommandId::StoragePreview,
            "Storage preview",
            "Inspect derived data before cleanup",
            "Storage",
            &["disk", "cache", "cleanup"],
            Action::Open(crate::component::Open::Storage),
            None,
        ),
        command(
            CommandId::TimestampAssistant,
            "Recognize timestamp",
            "Prepare a reviewed UTC timestamp enrichment proposal",
            "Agent",
            &["date", "time", "format", "brain"],
            Action::Open(crate::component::Open::Ask(
                crate::components::ask::AskOpen::Task(crate::app::AskTask::RecognizeTimestamp),
            )),
            view_reason,
        ),
        command(
            CommandId::AskAi,
            "Ask agent",
            "Open a typed filter or enrichment proposal",
            "Agent",
            &["assistant", "proposal"],
            Action::Open(crate::component::Open::Ask(
                crate::components::ask::AskOpen::Generic,
            )),
            view_reason,
        ),
        command(
            CommandId::AskAiFilter,
            "Ask agent for filter",
            "Select a filter proposal",
            "Agent",
            &["predicate proposal"],
            Action::None,
            Some("open Ask agent first"),
        ),
        command(
            CommandId::AskAiEnrichment,
            "Ask agent for enrichment",
            "Select an enrichment proposal",
            "Agent",
            &["derive proposal"],
            Action::None,
            Some("open Ask agent first"),
        ),
        command(
            CommandId::Investigations,
            "Investigations",
            "Open resumable investigation conversations",
            "Agent",
            &["sessions", "analysis"],
            Action::Open(crate::component::Open::Investigation),
            view_reason,
        ),
        command(
            CommandId::NewInvestigation,
            "New investigation",
            "Start a new investigation draft",
            "Agent",
            &["question", "conversation"],
            Action::None,
            Some("open Investigations first"),
        ),
        command(
            CommandId::ResumeInvestigation,
            "Resume selected investigation",
            "Resume the selected saved session",
            "Agent",
            &["continue session", "history"],
            Action::None,
            Some("open Investigations and select a saved session first"),
        ),
        command(
            CommandId::InvestigationFollowup,
            "Send investigation follow-up",
            "Send the current prompt to the active session",
            "Agent",
            &["reply", "continue conversation"],
            Action::None,
            Some("open an active investigation and enter a follow-up first"),
        ),
        command(
            CommandId::Settings,
            "Settings",
            "Edit global agent, appearance, and cache settings",
            "Application",
            &["preferences", "theme", "provider", "cache"],
            Action::Open(crate::component::Open::Settings),
            None,
        ),
        command(
            CommandId::Help,
            "Help",
            "Show keyboard help",
            "Application",
            &["keys", "shortcuts"],
            Action::Open(crate::component::Open::Help),
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
    // §4.3: a component's own entries, spliced in beside the shell command that
    // opens it. Their shortcut and availability come from the component; the
    // palette does not look inside it.
    for (layer, entry) in &context.layer_commands {
        // A command the catalog already lists is *taken over* by the layer
        // rather than duplicated: an operation the shell and a layer can both
        // reach — completion, which the enrichment editor still owns — stays
        // one row, and the row routes wherever it is currently available.
        if let Some(existing) = commands
            .iter_mut()
            .find(|command| command.id == entry.spec.id)
        {
            if entry.unavailable_reason.is_none() {
                existing.action = Action::Command(*layer, entry.spec.id);
                existing.shortcut = entry.spec.shortcut;
                existing.unavailable_reason = None;
            }
            continue;
        }
        let at = commands
            .iter()
            .position(|command| command.id == layer.palette_anchor())
            .map_or(commands.len(), |index| index + 1);
        commands.insert(
            at,
            Command {
                id: entry.spec.id,
                name: entry.spec.name,
                description: entry.spec.description,
                category: entry.spec.category,
                aliases: entry.spec.aliases,
                action: Action::Command(*layer, entry.spec.id),
                shortcut: entry.spec.shortcut,
                unavailable_reason: entry.unavailable_reason,
            },
        );
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
    (KeyCode::Char(','), KeyModifiers::NONE, ","),
    (KeyCode::Enter, KeyModifiers::NONE, "Enter"),
    (KeyCode::Char('g'), KeyModifiers::NONE, "g"),
    (KeyCode::Char('G'), KeyModifiers::SHIFT, "G"),
    (KeyCode::Char('}'), KeyModifiers::NONE, "}"),
    (KeyCode::Char('{'), KeyModifiers::NONE, "{"),
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
    (KeyCode::Char('o'), KeyModifiers::NONE, "o"),
    (KeyCode::Char('b'), KeyModifiers::NONE, "b"),
    (KeyCode::Char('B'), KeyModifiers::SHIFT, "B"),
    (KeyCode::Char('c'), KeyModifiers::NONE, "c"),
    (KeyCode::Char('x'), KeyModifiers::NONE, "x"),
    (KeyCode::Char(' '), KeyModifiers::NONE, "Space"),
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
    (KeyCode::Char('z'), KeyModifiers::NONE, "z"),
];

fn shortcut_for(action: &Action, focus: Focus) -> Option<&'static str> {
    // `o` is one key in both directions (raw-context-as-jump.md): the keymap
    // yields the jump, and the shell turns it into the return while an
    // origin is held, so the return row prints the chord that works.
    if *action == Action::ReturnFromRawContext {
        return matches!(focus, Focus::Logs | Focus::Selector).then_some("o");
    }
    SHORTCUT_CANDIDATES
        .iter()
        .find_map(|(code, modifiers, label)| {
            let key = KeyEvent::new(*code, *modifiers);
            (&key_to_action(key, focus) == action).then_some(*label)
        })
}
