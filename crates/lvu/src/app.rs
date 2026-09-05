use std::collections::HashMap;

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;

use crate::provider::{DisplayRow, RowId, RowProvider, ViewportRequest};

pub const MAX_EDITOR_BYTES: usize = 16 * 1024;
pub const MAX_PENDING_QUERY_REQUESTS: usize = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Focus {
    Selector,
    Logs,
    Editor,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceItem {
    pub id: String,
    pub name: String,
    pub health: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ViewItem {
    pub id: String,
    pub source_id: String,
    pub name: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ViewQueryState {
    pub draft: String,
    pub last_applied: String,
    pub error: Option<String>,
    pub pending_generation: Option<u64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ViewState {
    pub top: usize,
    pub selected: Option<RowId>,
    pub follow: bool,
    pub last_total: usize,
    pub provider_revision: u64,
    pub viewport_height: usize,
    pub query: ViewQueryState,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryRequest {
    pub view_id: String,
    pub generation: u64,
    pub draft: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryCompletion {
    pub view_id: String,
    pub generation: u64,
    pub result: Result<String, String>,
}

#[derive(Clone, Debug, Default)]
pub struct HitRegions {
    pub log: Option<Rect>,
    pub log_rows: Option<Rect>,
    pub sidebar: Option<Rect>,
    pub sidebar_views: Vec<(Rect, usize)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Action {
    Quit,
    CycleFocus,
    NextView,
    PreviousView,
    SelectSidebar(i32),
    MoveLine(i32),
    MovePage(i32),
    Top,
    End,
    ToggleDetails,
    ToggleHelp,
    ToggleFollow,
    OpenEditor,
    EditorInput(char),
    EditorBackspace,
    EditorPaste(String),
    SubmitDraft,
    CancelEditor,
    Resize(u16, u16),
    Mouse(MouseEvent),
    FixtureAdvance,
    None,
}

pub struct App {
    pub title: String,
    pub demo_mode: bool,
    pub sources: Vec<SourceItem>,
    pub views: Vec<ViewItem>,
    pub selected_view: usize,
    pub focus: Focus,
    pub show_details: bool,
    pub show_help: bool,
    pub terminal_size: (u16, u16),
    pub should_quit: bool,
    pub hit_regions: HitRegions,
    view_states: HashMap<String, ViewState>,
    query_requests: HashMap<String, QueryRequest>,
    next_query_generation: u64,
}

impl App {
    pub fn new(sources: Vec<SourceItem>, views: Vec<ViewItem>, demo_mode: bool) -> Self {
        let view_states = views
            .iter()
            .map(|view| {
                (
                    view.id.clone(),
                    ViewState {
                        follow: true,
                        ..ViewState::default()
                    },
                )
            })
            .collect();
        Self {
            title: "lvu log workspace".into(),
            demo_mode,
            sources,
            views,
            selected_view: 0,
            focus: Focus::Logs,
            show_details: false,
            show_help: false,
            terminal_size: (80, 24),
            should_quit: false,
            hit_regions: HitRegions::default(),
            view_states,
            query_requests: HashMap::new(),
            next_query_generation: 1,
        }
    }

    pub fn active_view_id(&self) -> Option<&str> {
        self.views
            .get(self.selected_view)
            .map(|view| view.id.as_str())
    }

    pub fn view_state(&self) -> Option<&ViewState> {
        self.active_view_id()
            .and_then(|id| self.view_states.get(id))
    }

    pub fn query_state(&self) -> Option<&ViewQueryState> {
        self.view_state().map(|state| &state.query)
    }

    pub fn visible_rows<P: RowProvider>(&self, provider: &P) -> Vec<DisplayRow> {
        let (Some(view_id), Some(state)) = (self.active_view_id(), self.view_state()) else {
            return Vec::new();
        };
        provider
            .page(
                view_id,
                ViewportRequest {
                    start: state.top,
                    len: state.viewport_height,
                },
            )
            .rows
    }

    pub fn selected_row<P: RowProvider>(&self, provider: &P) -> Option<DisplayRow> {
        let view_id = self.active_view_id()?;
        provider.row_by_id(view_id, self.view_state()?.selected.as_ref()?)
    }

    /// Recalculates the viewport even when provider content is unchanged.
    pub fn sync_provider<P: RowProvider>(&mut self, provider: &P, viewport_height: usize) -> bool {
        let Some(view_id) = self.active_view_id().map(str::to_owned) else {
            return false;
        };
        let revision = provider.revision(&view_id);
        let total = provider
            .page(&view_id, ViewportRequest { start: 0, len: 0 })
            .total;
        let height = viewport_height.max(1);
        let state = self
            .view_states
            .get_mut(&view_id)
            .expect("view state exists");
        let changed = revision != state.provider_revision
            || total != state.last_total
            || height != state.viewport_height;
        if !changed {
            return false;
        }
        state.provider_revision = revision;
        state.last_total = total;
        state.viewport_height = height;
        if total == 0 {
            state.top = 0;
            state.selected = None;
        } else if state.follow {
            state.top = total.saturating_sub(height);
            state.selected = provider
                .page(
                    &view_id,
                    ViewportRequest {
                        start: total - 1,
                        len: 1,
                    },
                )
                .rows
                .first()
                .map(|row| row.id.clone());
        } else {
            let selected_index = state
                .selected
                .as_ref()
                .and_then(|id| provider.index_of_id(&view_id, id));
            state.top = state.top.min(total - 1);
            if let Some(index) = selected_index {
                if index < state.top {
                    state.top = index;
                }
                if index >= state.top + height {
                    state.top = index + 1 - height;
                }
            } else {
                state.selected = provider
                    .page(
                        &view_id,
                        ViewportRequest {
                            start: state.top,
                            len: 1,
                        },
                    )
                    .rows
                    .first()
                    .map(|row| row.id.clone());
            }
        }
        true
    }

    /// At most one unsent request per view is retained.
    pub fn take_query_requests(&mut self) -> Vec<QueryRequest> {
        self.query_requests
            .drain()
            .map(|(_, request)| request)
            .collect()
    }

    pub fn apply_query_completion(&mut self, completion: QueryCompletion) -> bool {
        let Some(state) = self.view_states.get_mut(&completion.view_id) else {
            return false;
        };
        if state.query.pending_generation != Some(completion.generation) {
            return false;
        }
        state.query.pending_generation = None;
        match completion.result {
            Ok(applied) => {
                state.query.last_applied = applied;
                state.query.error = None;
            }
            Err(error) => state.query.error = Some(error),
        }
        true
    }

    pub fn handle<P: RowProvider>(&mut self, action: Action, provider: &P) {
        match action {
            Action::Quit => self.should_quit = true,
            Action::CycleFocus => {
                self.focus = match self.focus {
                    Focus::Selector => Focus::Logs,
                    Focus::Logs if !self.views.is_empty() => Focus::Selector,
                    Focus::Logs | Focus::Editor => Focus::Logs,
                }
            }
            Action::NextView | Action::SelectSidebar(1) => self.switch_view(1, provider),
            Action::PreviousView | Action::SelectSidebar(-1) => self.switch_view(-1, provider),
            Action::SelectSidebar(_) => {}
            Action::MoveLine(delta) => self.move_selection(delta, provider),
            Action::MovePage(delta) => {
                let height = self
                    .view_state()
                    .map_or(1, |state| state.viewport_height.max(1));
                self.move_selection(delta * height as i32, provider);
            }
            Action::Top => self.select_index(0, provider),
            Action::End => {
                if let Some(total) = self.view_state().map(|state| state.last_total)
                    && total > 0
                {
                    self.select_index(total - 1, provider);
                }
            }
            Action::ToggleDetails => self.show_details = !self.show_details,
            Action::ToggleHelp => self.show_help = !self.show_help,
            Action::ToggleFollow => self.toggle_follow(provider),
            Action::OpenEditor => {
                if self.active_view_id().is_some() {
                    self.focus = Focus::Editor;
                }
            }
            Action::EditorInput(character) if self.focus == Focus::Editor => {
                self.append_editor(&character.to_string())
            }
            Action::EditorBackspace if self.focus == Focus::Editor => {
                if let Some(id) = self.active_view_id().map(str::to_owned) {
                    self.view_states
                        .get_mut(&id)
                        .expect("view state")
                        .query
                        .draft
                        .pop();
                }
            }
            Action::EditorPaste(text) if self.focus == Focus::Editor => self.append_editor(&text),
            Action::SubmitDraft if self.focus == Focus::Editor => self.submit_draft(),
            Action::CancelEditor => self.focus = Focus::Logs,
            Action::Resize(width, height) => self.terminal_size = (width, height),
            Action::Mouse(event) => self.handle_mouse(event, provider),
            Action::FixtureAdvance | Action::None => {}
            Action::EditorInput(_)
            | Action::EditorBackspace
            | Action::EditorPaste(_)
            | Action::SubmitDraft => {}
        }
    }

    fn append_editor(&mut self, text: &str) {
        let Some(id) = self.active_view_id().map(str::to_owned) else {
            return;
        };
        let draft = &mut self
            .view_states
            .get_mut(&id)
            .expect("view state")
            .query
            .draft;
        let remaining = MAX_EDITOR_BYTES.saturating_sub(draft.len());
        let mut end = text.len().min(remaining);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        draft.push_str(&text[..end]);
    }

    fn submit_draft(&mut self) {
        let Some(view_id) = self.active_view_id().map(str::to_owned) else {
            return;
        };
        if !self.query_requests.contains_key(&view_id)
            && self.query_requests.len() >= MAX_PENDING_QUERY_REQUESTS
        {
            self.view_states
                .get_mut(&view_id)
                .expect("view state")
                .query
                .error = Some("query submission queue is full; draft was preserved".into());
            return;
        }
        let generation = self.next_query_generation;
        self.next_query_generation = self.next_query_generation.saturating_add(1);
        let state = self.view_states.get_mut(&view_id).expect("view state");
        state.query.pending_generation = Some(generation);
        state.query.error = None;
        self.query_requests.insert(
            view_id.clone(),
            QueryRequest {
                view_id,
                generation,
                draft: state.query.draft.clone(),
            },
        );
    }

    fn switch_view<P: RowProvider>(&mut self, delta: i32, provider: &P) {
        if self.views.is_empty() {
            return;
        }
        self.selected_view =
            (self.selected_view as i32 + delta).rem_euclid(self.views.len() as i32) as usize;
        let height = self
            .view_state()
            .map_or(1, |state| state.viewport_height.max(1));
        self.sync_provider(provider, height);
    }

    fn selected_index<P: RowProvider>(&self, provider: &P) -> usize {
        let (Some(view_id), Some(state)) = (self.active_view_id(), self.view_state()) else {
            return 0;
        };
        state
            .selected
            .as_ref()
            .and_then(|id| provider.index_of_id(view_id, id))
            .unwrap_or(state.top)
    }

    fn move_selection<P: RowProvider>(&mut self, delta: i32, provider: &P) {
        let Some(total) = self.view_state().map(|state| state.last_total) else {
            return;
        };
        if total == 0 {
            return;
        }
        let target = (self.selected_index(provider) as i64 + i64::from(delta))
            .clamp(0, total.saturating_sub(1) as i64);
        self.select_index(target as usize, provider);
    }

    fn select_index<P: RowProvider>(&mut self, index: usize, provider: &P) {
        let Some(view_id) = self.active_view_id().map(str::to_owned) else {
            return;
        };
        let total = self.view_states[&view_id].last_total;
        if total == 0 {
            return;
        }
        let index = index.min(total - 1);
        let selected = provider
            .page(
                &view_id,
                ViewportRequest {
                    start: index,
                    len: 1,
                },
            )
            .rows
            .first()
            .map(|row| row.id.clone());
        let state = self.view_states.get_mut(&view_id).expect("view state");
        let height = state.viewport_height.max(1);
        state.selected = selected;
        if index < state.top {
            state.top = index;
        } else if index >= state.top + height {
            state.top = index + 1 - height;
        }
        state.follow = index + 1 == total;
    }

    fn toggle_follow<P: RowProvider>(&mut self, provider: &P) {
        let Some(id) = self.active_view_id().map(str::to_owned) else {
            return;
        };
        let follow = !self.view_states[&id].follow;
        self.view_states.get_mut(&id).expect("view state").follow = follow;
        if follow {
            self.handle(Action::End, provider);
        }
    }

    fn handle_mouse<P: RowProvider>(&mut self, event: MouseEvent, provider: &P) {
        if self.focus == Focus::Editor {
            return;
        }
        if self.show_help {
            if matches!(event.kind, MouseEventKind::Down(MouseButton::Left)) {
                self.show_help = false;
            }
            return;
        }
        let point = (event.column, event.row);
        if matches!(event.kind, MouseEventKind::Down(MouseButton::Left)) {
            if let Some((_, index)) = self
                .hit_regions
                .sidebar_views
                .iter()
                .find(|(area, _)| contains(*area, point))
            {
                self.selected_view = *index;
                self.focus = Focus::Selector;
                let height = self
                    .view_state()
                    .map_or(1, |state| state.viewport_height.max(1));
                self.sync_provider(provider, height);
                return;
            }
            if let Some(rows) = self
                .hit_regions
                .log_rows
                .filter(|area| contains(*area, point))
            {
                let index = self.view_state().map_or(0, |state| state.top)
                    + usize::from(event.row - rows.y);
                self.focus = Focus::Logs;
                self.select_index(index, provider);
            }
            return;
        }
        let over_log = self
            .hit_regions
            .log
            .is_some_and(|area| contains(area, point));
        let over_sidebar = self
            .hit_regions
            .sidebar
            .is_some_and(|area| contains(area, point));
        match event.kind {
            MouseEventKind::ScrollUp if over_log => self.move_selection(-3, provider),
            MouseEventKind::ScrollDown if over_log => self.move_selection(3, provider),
            MouseEventKind::ScrollUp if over_sidebar => self.switch_view(-1, provider),
            MouseEventKind::ScrollDown if over_sidebar => self.switch_view(1, provider),
            _ => {}
        }
    }
}

fn contains(area: Rect, point: (u16, u16)) -> bool {
    point.0 >= area.x && point.0 < area.right() && point.1 >= area.y && point.1 < area.bottom()
}

pub fn key_to_action(key: KeyEvent, focus: Focus) -> Action {
    if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
        return Action::None;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Action::Quit;
    }
    if focus == Focus::Editor {
        return match key.code {
            KeyCode::Esc => Action::CancelEditor,
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::ALT) => {
                Action::EditorInput('\n')
            }
            KeyCode::Enter => Action::SubmitDraft,
            KeyCode::Backspace => Action::EditorBackspace,
            KeyCode::Char(character) => Action::EditorInput(character),
            _ => Action::None,
        };
    }
    if focus == Focus::Selector {
        return match key.code {
            KeyCode::Down | KeyCode::Char('j') => Action::SelectSidebar(1),
            KeyCode::Up | KeyCode::Char('k') => Action::SelectSidebar(-1),
            KeyCode::Tab => Action::CycleFocus,
            KeyCode::Char('q') => Action::Quit,
            KeyCode::Char('?') => Action::ToggleHelp,
            _ => Action::None,
        };
    }
    match key.code {
        KeyCode::Char('q') => Action::Quit,
        KeyCode::Tab => Action::CycleFocus,
        KeyCode::Char(']') => Action::NextView,
        KeyCode::Char('[') => Action::PreviousView,
        KeyCode::Down | KeyCode::Char('j') => Action::MoveLine(1),
        KeyCode::Up | KeyCode::Char('k') => Action::MoveLine(-1),
        KeyCode::PageDown => Action::MovePage(1),
        KeyCode::PageUp => Action::MovePage(-1),
        KeyCode::Home | KeyCode::Char('g') => Action::Top,
        KeyCode::End | KeyCode::Char('G') => Action::End,
        KeyCode::Char('d') => Action::ToggleDetails,
        KeyCode::Char('?') => Action::ToggleHelp,
        KeyCode::Char('f') => Action::ToggleFollow,
        KeyCode::Char('/') => Action::OpenEditor,
        KeyCode::Char('a') => Action::FixtureAdvance,
        _ => Action::None,
    }
}
