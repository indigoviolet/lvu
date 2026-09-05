use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
};

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;

use crate::provider::{DisplayRow, RowId, RowProvider, ViewportRequest};

pub const MAX_EDITOR_BYTES: usize = 16 * 1024;
pub const MAX_PENDING_QUERY_REQUESTS: usize = 32;
pub const SEARCH_DEBOUNCE: Duration = Duration::from_millis(120);
const MAX_SOURCE_REQUESTS: usize = 8;
const MAX_DISCOVERY_REQUESTS: usize = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Focus {
    Selector,
    Logs,
    SearchEditor,
    AdvancedEditor,
    SourceDialog,
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
pub struct EditorState {
    pub draft: String,
    pub applied: String,
    pub error: Option<String>,
    pub pending_generation: Option<u64>,
    pending_value: Option<String>,
    pending_revision: Option<u64>,
    search_due: Option<Instant>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ViewState {
    pub top: usize,
    pub selected: Option<RowId>,
    pub follow: bool,
    pub last_total: usize,
    pub provider_revision: u64,
    pub viewport_height: usize,
    pub search: EditorState,
    pub advanced: EditorState,
    pub applied_query_revision: u64,
    pub desired_query_revision: u64,
    desired_constraints: QueryConstraints,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum QueryPurpose {
    Search,
    Advanced,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// A literal substring constraint. Case-insensitive adapters use Rust's
/// locale-neutral Unicode lowercase mapping, not locale-specific case rules.
pub struct TextConstraint {
    pub literal: String,
    pub case_insensitive: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct QueryConstraints {
    pub text: Option<TextConstraint>,
    pub advanced_polars: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryRequest {
    pub view_id: String,
    pub generation: u64,
    /// Monotonic per-view revision of the complete AND-combined constraints.
    pub revision: u64,
    pub base_revision: u64,
    pub base_constraints: QueryConstraints,
    pub purpose: QueryPurpose,
    pub constraints: QueryConstraints,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryCompletion {
    pub view_id: String,
    pub generation: u64,
    pub revision: u64,
    pub purpose: QueryPurpose,
    pub result: Result<(), QueryFailure>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryFailure {
    /// The constraint which failed validation, independent of request purpose.
    pub purpose: QueryPurpose,
    pub message: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceKind {
    File,
    Command,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceLaunchRequest {
    pub kind: SourceKind,
    pub text: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceDialogState {
    pub kind: SourceKind,
    pub draft: String,
    pub error: Option<String>,
    pub mode: SourceDialogMode,
    pub discovery: DiscoveryDialogState,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SourceDialogMode {
    #[default]
    Manual,
    Discovery,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveryItem {
    pub key: String,
    pub label: String,
    pub detail: String,
    pub status: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DiscoveryDialogState {
    pub generation: u64,
    pub query: String,
    pub items: Vec<DiscoveryItem>,
    pub selected: usize,
    pub scanning: bool,
    pub status: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiscoveryUiRequest {
    Scan { generation: u64 },
    Cancel { generation: u64 },
    Select { generation: u64, key: String },
}

impl Default for SourceDialogState {
    fn default() -> Self {
        Self {
            kind: SourceKind::File,
            draft: String::new(),
            error: None,
            mode: SourceDialogMode::Manual,
            discovery: DiscoveryDialogState::default(),
        }
    }
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
    OpenSearch,
    OpenAdvanced,
    OpenSource,
    ToggleDiscovery,
    RefreshDiscovery,
    MoveDiscovery(i32),
    ToggleSourceKind,
    SourceInput(char),
    SourceBackspace,
    SubmitSource,
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
    pub source_dialog: Option<SourceDialogState>,
    pub source_notice: Option<String>,
    view_states: HashMap<String, ViewState>,
    query_requests: HashMap<(String, QueryPurpose), QueryRequest>,
    next_query_generation: u64,
    source_requests: VecDeque<SourceLaunchRequest>,
    discovery_requests: VecDeque<DiscoveryUiRequest>,
}

impl App {
    pub fn new(sources: Vec<SourceItem>, views: Vec<ViewItem>, demo_mode: bool) -> Self {
        let empty = views.is_empty();
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
            focus: if empty {
                Focus::SourceDialog
            } else {
                Focus::Logs
            },
            show_details: false,
            show_help: false,
            terminal_size: (80, 24),
            should_quit: false,
            hit_regions: HitRegions::default(),
            source_dialog: empty.then(SourceDialogState::default),
            source_notice: None,
            view_states,
            query_requests: HashMap::new(),
            next_query_generation: 1,
            source_requests: VecDeque::new(),
            discovery_requests: VecDeque::new(),
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

    pub fn search_state(&self) -> Option<&EditorState> {
        self.view_state().map(|state| &state.search)
    }

    pub fn advanced_state(&self) -> Option<&EditorState> {
        self.view_state().map(|state| &state.advanced)
    }

    pub fn active_editor_state(&self) -> Option<&EditorState> {
        match self.focus {
            Focus::SearchEditor => self.search_state(),
            Focus::AdvancedEditor => self.advanced_state(),
            Focus::Selector | Focus::Logs | Focus::SourceDialog => None,
        }
    }

    pub fn take_source_requests(&mut self) -> Vec<SourceLaunchRequest> {
        self.source_requests.drain(..).collect()
    }

    pub fn take_discovery_requests(&mut self) -> Vec<DiscoveryUiRequest> {
        self.discovery_requests.drain(..).collect()
    }

    pub fn apply_discovery_result(
        &mut self,
        generation: u64,
        items: Vec<DiscoveryItem>,
        status: String,
    ) -> bool {
        let Some(dialog) = &mut self.source_dialog else {
            return false;
        };
        if dialog.discovery.generation != generation {
            return false;
        }
        dialog.discovery.items = items;
        dialog.discovery.selected = 0;
        dialog.discovery.scanning = false;
        dialog.discovery.status = status;
        dialog.error = None;
        true
    }

    pub fn add_source_view(&mut self, source: SourceItem, view: ViewItem) {
        if self.sources.iter().all(|item| item.id != source.id) {
            self.sources.push(source);
        }
        if self.views.iter().all(|item| item.id != view.id) {
            self.view_states.insert(
                view.id.clone(),
                ViewState {
                    follow: true,
                    ..ViewState::default()
                },
            );
            self.views.push(view);
        }
        if self.views.len() == 1 {
            self.selected_view = 0;
        }
    }

    pub fn select_view(&mut self, view_id: &str) {
        if let Some(index) = self.views.iter().position(|view| view.id == view_id) {
            self.selected_view = index;
            self.focus = Focus::Logs;
        }
    }

    pub fn source_request_succeeded(&mut self, request: &SourceLaunchRequest, view_id: &str) {
        self.source_notice = Some("source started".into());
        self.select_view(view_id);
        if self
            .source_dialog
            .as_ref()
            .is_some_and(|dialog| dialog.kind == request.kind && dialog.draft == request.text)
        {
            self.source_dialog = None;
            self.focus = Focus::Logs;
        }
    }

    pub fn source_request_failed(&mut self, request: SourceLaunchRequest, message: String) {
        self.source_notice = Some(format!("source error: {message}"));
        match &mut self.source_dialog {
            Some(dialog) if dialog.kind == request.kind && dialog.draft == request.text => {
                dialog.error = Some(message);
            }
            Some(_) => {}
            None => {
                self.source_dialog = Some(SourceDialogState {
                    kind: request.kind,
                    draft: request.text,
                    error: Some(message),
                    mode: SourceDialogMode::Manual,
                    discovery: DiscoveryDialogState::default(),
                });
                self.focus = Focus::SourceDialog;
            }
        }
    }

    pub fn discovery_selection_succeeded(&mut self, generation: u64, view_id: &str) {
        self.source_notice = Some("discovered source started".into());
        self.select_view(view_id);
        if self.source_dialog.as_ref().is_some_and(|dialog| {
            dialog.mode == SourceDialogMode::Discovery && dialog.discovery.generation == generation
        }) {
            self.source_dialog = None;
            self.focus = Focus::Logs;
        }
    }

    pub fn discovery_selection_failed(&mut self, generation: u64, message: String) {
        self.source_notice = Some(format!("source error: {message}"));
        if let Some(dialog) = &mut self.source_dialog
            && dialog.mode == SourceDialogMode::Discovery
            && dialog.discovery.generation == generation
        {
            dialog.error = Some(message);
        }
    }

    pub fn update_source_health(&mut self, source_id: &str, health: String) {
        if let Some(source) = self
            .sources
            .iter_mut()
            .find(|source| source.id == source_id)
        {
            source.health = health;
        }
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
            } else if state.selected.is_none() {
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

    /// At most one unsent request per view and purpose is retained.
    pub fn take_query_requests(&mut self) -> Vec<QueryRequest> {
        self.query_requests
            .drain()
            .map(|(_, request)| request)
            .collect()
    }

    /// Enqueues due live searches. Tests pass a future instant to avoid sleeps.
    pub fn flush_debounced_searches(&mut self, now: Instant) -> bool {
        let due: Vec<String> = self
            .view_states
            .iter()
            .filter(|(_, state)| {
                state
                    .search
                    .search_due
                    .is_some_and(|deadline| deadline <= now)
            })
            .map(|(view_id, _)| view_id.clone())
            .collect();
        for view_id in &due {
            self.enqueue_query(view_id, QueryPurpose::Search);
            self.view_states
                .get_mut(view_id)
                .expect("view state")
                .search
                .search_due = None;
        }
        !due.is_empty()
    }

    pub fn apply_query_completion(&mut self, completion: QueryCompletion) -> bool {
        let Some(state) = self.view_states.get_mut(&completion.view_id) else {
            return false;
        };
        if completion.revision != state.desired_query_revision {
            return false;
        }
        let editor = editor_mut(state, completion.purpose);
        if editor.pending_generation != Some(completion.generation) {
            return false;
        }
        match completion.result {
            Ok(()) => {
                let constraints = state.desired_constraints.clone();
                let accepted_search = pending_at_or_before(&state.search, completion.revision);
                let accepted_advanced = pending_at_or_before(&state.advanced, completion.revision);
                state.search.applied = constraint_text(&constraints);
                state.advanced.applied = constraints.advanced_polars.clone().unwrap_or_default();
                state.applied_query_revision = completion.revision;
                clear_accepted_pending(&mut state.search, completion.revision);
                clear_accepted_pending(&mut state.advanced, completion.revision);
                if accepted_search {
                    state.search.error = None;
                }
                if accepted_advanced {
                    state.advanced.error = None;
                }
            }
            Err(failure) => {
                let counterpart = match failure.purpose {
                    QueryPurpose::Search => {
                        pending_at_or_before(&state.advanced, completion.revision)
                            .then(|| {
                                state
                                    .advanced
                                    .pending_value
                                    .clone()
                                    .map(|value| (QueryPurpose::Advanced, value))
                            })
                            .flatten()
                    }
                    QueryPurpose::Advanced => {
                        pending_at_or_before(&state.search, completion.revision)
                            .then(|| {
                                state
                                    .search
                                    .pending_value
                                    .clone()
                                    .map(|value| (QueryPurpose::Search, value))
                            })
                            .flatten()
                    }
                };
                let editor = editor_mut(state, failure.purpose);
                editor.pending_generation = None;
                editor.pending_revision = None;
                editor.pending_value = None;
                editor.error = Some(failure.message);
                state.desired_constraints = applied_constraints(state);
                if let Some((purpose, value)) = counterpart {
                    // The older counterpart was never allowed to publish. Rebase it
                    // on the last accepted constraint and give it a fresh revision.
                    self.enqueue_query_value(&completion.view_id, purpose, Some(value));
                }
            }
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
                    Focus::Logs
                    | Focus::SearchEditor
                    | Focus::AdvancedEditor
                    | Focus::SourceDialog => Focus::Logs,
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
            Action::OpenSearch => {
                if self.active_view_id().is_some() {
                    self.focus = Focus::SearchEditor;
                }
            }
            Action::OpenAdvanced => {
                if self.active_view_id().is_some() {
                    self.focus = Focus::AdvancedEditor;
                }
            }
            Action::OpenSource => {
                self.source_dialog.get_or_insert_with(Default::default);
                self.focus = Focus::SourceDialog;
            }
            Action::ToggleDiscovery if self.focus == Focus::SourceDialog => {
                let dialog = self.source_dialog.as_mut().expect("source dialog");
                dialog.mode = match dialog.mode {
                    SourceDialogMode::Manual => SourceDialogMode::Discovery,
                    SourceDialogMode::Discovery => SourceDialogMode::Manual,
                };
                if dialog.mode == SourceDialogMode::Discovery && dialog.discovery.generation == 0 {
                    self.start_discovery_scan();
                }
            }
            Action::RefreshDiscovery if self.focus == Focus::SourceDialog => {
                self.start_discovery_scan();
            }
            Action::MoveDiscovery(delta) if self.focus == Focus::SourceDialog => {
                self.move_discovery(delta);
            }
            Action::ToggleSourceKind if self.focus == Focus::SourceDialog => {
                if let Some(dialog) = &mut self.source_dialog {
                    if dialog.mode == SourceDialogMode::Discovery {
                        return;
                    }
                    dialog.kind = match dialog.kind {
                        SourceKind::File => SourceKind::Command,
                        SourceKind::Command => SourceKind::File,
                    };
                    dialog.error = None;
                }
            }
            Action::SourceInput(character) if self.focus == Focus::SourceDialog => {
                if self
                    .source_dialog
                    .as_ref()
                    .is_some_and(|dialog| dialog.mode == SourceDialogMode::Discovery)
                {
                    self.append_discovery_query(&character.to_string());
                } else {
                    self.append_source(&character.to_string());
                }
            }
            Action::SourceBackspace if self.focus == Focus::SourceDialog => {
                if let Some(dialog) = &mut self.source_dialog {
                    if dialog.mode == SourceDialogMode::Discovery {
                        dialog.discovery.query.pop();
                        dialog.discovery.selected = 0;
                    } else {
                        dialog.draft.pop();
                    }
                    dialog.error = None;
                }
            }
            Action::SubmitSource if self.focus == Focus::SourceDialog => {
                if self
                    .source_dialog
                    .as_ref()
                    .is_some_and(|dialog| dialog.mode == SourceDialogMode::Discovery)
                {
                    self.submit_discovered_source();
                } else {
                    self.submit_source();
                }
            }
            Action::EditorInput(character) if self.editor_open() => {
                self.append_editor(&character.to_string())
            }
            Action::EditorBackspace if self.editor_open() => {
                self.edit_active(|editor| {
                    editor.draft.pop();
                });
                self.schedule_search();
            }
            Action::EditorPaste(text) if self.focus == Focus::SourceDialog => {
                if self
                    .source_dialog
                    .as_ref()
                    .is_some_and(|dialog| dialog.mode == SourceDialogMode::Discovery)
                {
                    self.append_discovery_query(&text);
                } else {
                    self.append_source(&text);
                }
            }
            Action::EditorPaste(text) if self.editor_open() => self.append_editor(&text),
            Action::SubmitDraft if self.editor_open() => self.submit_draft(),
            Action::CancelEditor => {
                if self.focus == Focus::SourceDialog {
                    if let Some(dialog) = &self.source_dialog
                        && dialog.discovery.scanning
                        && self.discovery_requests.len() < MAX_DISCOVERY_REQUESTS
                    {
                        self.discovery_requests
                            .push_back(DiscoveryUiRequest::Cancel {
                                generation: dialog.discovery.generation,
                            });
                    }
                    self.source_dialog = None;
                }
                self.focus = Focus::Logs;
            }
            Action::Resize(width, height) => self.terminal_size = (width, height),
            Action::Mouse(event) => self.handle_mouse(event, provider),
            Action::FixtureAdvance | Action::None => {}
            Action::EditorInput(_)
            | Action::EditorBackspace
            | Action::EditorPaste(_)
            | Action::SubmitDraft => {}
            Action::ToggleSourceKind
            | Action::ToggleDiscovery
            | Action::RefreshDiscovery
            | Action::MoveDiscovery(_)
            | Action::SourceInput(_)
            | Action::SourceBackspace
            | Action::SubmitSource => {}
        }
    }

    fn append_source(&mut self, text: &str) {
        let Some(dialog) = &mut self.source_dialog else {
            return;
        };
        let remaining = MAX_EDITOR_BYTES.saturating_sub(dialog.draft.len());
        let mut end = text.len().min(remaining);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        dialog.draft.push_str(&text[..end]);
        dialog.error = None;
    }

    fn append_discovery_query(&mut self, text: &str) {
        let Some(discovery) = self
            .source_dialog
            .as_mut()
            .map(|dialog| &mut dialog.discovery)
        else {
            return;
        };
        let remaining = MAX_EDITOR_BYTES.saturating_sub(discovery.query.len());
        let mut end = text.len().min(remaining);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        discovery.query.push_str(&text[..end]);
        discovery.selected = 0;
    }

    fn start_discovery_scan(&mut self) {
        let Some(discovery) = self
            .source_dialog
            .as_mut()
            .map(|dialog| &mut dialog.discovery)
        else {
            return;
        };
        if discovery.generation > 0 && discovery.scanning {
            self.discovery_requests
                .push_back(DiscoveryUiRequest::Cancel {
                    generation: discovery.generation,
                });
        }
        discovery.generation = discovery.generation.saturating_add(1).max(1);
        discovery.scanning = true;
        discovery.items.clear();
        discovery.selected = 0;
        discovery.status = "scanning bounded local providers…".into();
        if self.discovery_requests.len() < MAX_DISCOVERY_REQUESTS {
            self.discovery_requests.push_back(DiscoveryUiRequest::Scan {
                generation: discovery.generation,
            });
        } else {
            discovery.scanning = false;
            discovery.status = "discovery request queue is full".into();
        }
    }

    fn move_discovery(&mut self, delta: i32) {
        let Some(dialog) = &mut self.source_dialog else {
            return;
        };
        let count = filtered_discovery_indices(&dialog.discovery).len();
        if count == 0 {
            dialog.discovery.selected = 0;
            return;
        }
        dialog.discovery.selected = dialog
            .discovery
            .selected
            .saturating_add_signed(delta as isize)
            .min(count - 1);
    }

    fn submit_discovered_source(&mut self) {
        let Some(dialog) = &mut self.source_dialog else {
            return;
        };
        let indices = filtered_discovery_indices(&dialog.discovery);
        let Some(index) = indices.get(dialog.discovery.selected).copied() else {
            dialog.error = Some("no matching discovered source to start".into());
            return;
        };
        if self.discovery_requests.len() >= MAX_DISCOVERY_REQUESTS {
            dialog.error = Some("discovery action queue is full".into());
            return;
        }
        self.discovery_requests
            .push_back(DiscoveryUiRequest::Select {
                generation: dialog.discovery.generation,
                key: dialog.discovery.items[index].key.clone(),
            });
        dialog.error = Some("starting selected source…".into());
    }

    fn submit_source(&mut self) {
        let Some(dialog) = &mut self.source_dialog else {
            return;
        };
        if dialog.draft.is_empty() {
            dialog.error = Some("enter a file path or shell command".into());
            return;
        }
        if self.source_requests.len() >= MAX_SOURCE_REQUESTS {
            dialog.error = Some("source launch queue is full".into());
            return;
        }
        self.source_requests.push_back(SourceLaunchRequest {
            kind: dialog.kind,
            text: dialog.draft.clone(),
        });
        dialog.error = Some("starting source…".into());
    }

    fn append_editor(&mut self, text: &str) {
        let Some(id) = self.active_view_id().map(str::to_owned) else {
            return;
        };
        let purpose = self.editor_purpose().expect("editor open");
        let state = self.view_states.get_mut(&id).expect("view state");
        let draft = match purpose {
            QueryPurpose::Search => &mut state.search.draft,
            QueryPurpose::Advanced => &mut state.advanced.draft,
        };
        let remaining = MAX_EDITOR_BYTES.saturating_sub(draft.len());
        let mut end = text.len().min(remaining);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        draft.push_str(&text[..end]);
        self.schedule_search();
    }

    fn submit_draft(&mut self) {
        let Some(view_id) = self.active_view_id().map(str::to_owned) else {
            return;
        };
        let Some(purpose) = self.editor_purpose() else {
            return;
        };
        if purpose == QueryPurpose::Search {
            self.view_states
                .get_mut(&view_id)
                .expect("view state")
                .search
                .search_due = None;
        }
        self.enqueue_query(&view_id, purpose);
    }

    fn enqueue_query(&mut self, view_id: &str, purpose: QueryPurpose) {
        self.enqueue_query_value(view_id, purpose, None);
    }

    fn enqueue_query_value(&mut self, view_id: &str, purpose: QueryPurpose, value: Option<String>) {
        let key = (view_id.to_owned(), purpose);
        if !self.query_requests.contains_key(&key)
            && self.query_requests.len() >= MAX_PENDING_QUERY_REQUESTS
        {
            self.editor_mut(view_id, purpose).error =
                Some("query submission queue is full; draft was preserved".into());
            return;
        }
        let generation = self.next_query_generation;
        self.next_query_generation = self.next_query_generation.saturating_add(1);
        let state = self.view_states.get_mut(view_id).expect("view state");
        let base_revision = state.applied_query_revision;
        let base_constraints = applied_constraints(state);
        let mut constraints = state.desired_constraints.clone();
        let pending_value = match purpose {
            QueryPurpose::Search => {
                let value = value.unwrap_or_else(|| state.search.draft.clone());
                constraints.text = nonempty_text(&value);
                value
            }
            QueryPurpose::Advanced => {
                let value = value.unwrap_or_else(|| state.advanced.draft.clone());
                constraints.advanced_polars = nonempty(&value);
                value
            }
        };
        state.desired_query_revision = state.desired_query_revision.saturating_add(1);
        let revision = state.desired_query_revision;
        state.desired_constraints = constraints.clone();
        let editor = match purpose {
            QueryPurpose::Search => &mut state.search,
            QueryPurpose::Advanced => &mut state.advanced,
        };
        editor.pending_generation = Some(generation);
        editor.pending_revision = Some(revision);
        editor.pending_value = Some(pending_value);
        editor.error = None;
        self.query_requests.insert(
            key,
            QueryRequest {
                view_id: view_id.to_owned(),
                generation,
                revision,
                base_revision,
                base_constraints,
                purpose,
                constraints,
            },
        );
    }

    fn editor_open(&self) -> bool {
        matches!(self.focus, Focus::SearchEditor | Focus::AdvancedEditor)
    }

    fn editor_purpose(&self) -> Option<QueryPurpose> {
        match self.focus {
            Focus::SearchEditor => Some(QueryPurpose::Search),
            Focus::AdvancedEditor => Some(QueryPurpose::Advanced),
            Focus::Selector | Focus::Logs | Focus::SourceDialog => None,
        }
    }

    fn editor_mut(&mut self, view_id: &str, purpose: QueryPurpose) -> &mut EditorState {
        let state = self.view_states.get_mut(view_id).expect("view state");
        match purpose {
            QueryPurpose::Search => &mut state.search,
            QueryPurpose::Advanced => &mut state.advanced,
        }
    }

    fn edit_active(&mut self, edit: impl FnOnce(&mut EditorState)) {
        let (Some(view_id), Some(purpose)) = (
            self.active_view_id().map(str::to_owned),
            self.editor_purpose(),
        ) else {
            return;
        };
        edit(self.editor_mut(&view_id, purpose));
    }

    fn schedule_search(&mut self) {
        if self.focus != Focus::SearchEditor {
            return;
        }
        let Some(view_id) = self.active_view_id().map(str::to_owned) else {
            return;
        };
        self.view_states
            .get_mut(&view_id)
            .expect("view state")
            .search
            .search_due = Some(Instant::now() + SEARCH_DEBOUNCE);
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
        if self.editor_open() || self.focus == Focus::SourceDialog {
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

pub fn filtered_discovery_indices(state: &DiscoveryDialogState) -> Vec<usize> {
    let query = state.query.to_lowercase();
    state
        .items
        .iter()
        .enumerate()
        .filter(|(_, item)| {
            query.is_empty()
                || item.label.to_lowercase().contains(&query)
                || item.detail.to_lowercase().contains(&query)
                || item.status.to_lowercase().contains(&query)
        })
        .map(|(index, _)| index)
        .collect()
}

fn nonempty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_owned())
}

fn nonempty_text(value: &str) -> Option<TextConstraint> {
    (!value.is_empty()).then(|| TextConstraint {
        literal: value.to_owned(),
        case_insensitive: true,
    })
}

fn applied_constraints(state: &ViewState) -> QueryConstraints {
    QueryConstraints {
        text: nonempty_text(&state.search.applied),
        advanced_polars: nonempty(&state.advanced.applied),
    }
}

fn constraint_text(constraints: &QueryConstraints) -> String {
    constraints
        .text
        .as_ref()
        .map_or_else(String::new, |text| text.literal.clone())
}

fn editor_mut(state: &mut ViewState, purpose: QueryPurpose) -> &mut EditorState {
    match purpose {
        QueryPurpose::Search => &mut state.search,
        QueryPurpose::Advanced => &mut state.advanced,
    }
}

fn pending_at_or_before(editor: &EditorState, revision: u64) -> bool {
    editor
        .pending_revision
        .is_some_and(|pending| pending <= revision)
}

fn clear_accepted_pending(editor: &mut EditorState, revision: u64) {
    if editor
        .pending_revision
        .is_some_and(|pending| pending <= revision)
    {
        editor.pending_generation = None;
        editor.pending_revision = None;
        editor.pending_value = None;
    }
}

pub fn key_to_action(key: KeyEvent, focus: Focus) -> Action {
    if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
        return Action::None;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Action::Quit;
    }
    if matches!(focus, Focus::SearchEditor | Focus::AdvancedEditor) {
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
    if focus == Focus::SourceDialog {
        return match key.code {
            KeyCode::Esc => Action::CancelEditor,
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Action::ToggleDiscovery
            }
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Action::RefreshDiscovery
            }
            KeyCode::Down => Action::MoveDiscovery(1),
            KeyCode::Up => Action::MoveDiscovery(-1),
            KeyCode::Tab => Action::ToggleSourceKind,
            KeyCode::Enter => Action::SubmitSource,
            KeyCode::Backspace => Action::SourceBackspace,
            KeyCode::Char(character) => Action::SourceInput(character),
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
        KeyCode::Char('/') => Action::OpenSearch,
        KeyCode::Char('p') => Action::OpenAdvanced,
        KeyCode::Char('n') => Action::OpenSource,
        KeyCode::Char('a') => Action::FixtureAdvance,
        _ => Action::None,
    }
}
