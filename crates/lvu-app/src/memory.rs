use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::mpsc::{self, Receiver, SyncSender, TrySendError},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use lvu::app::RecipeOutcome;
use lvu::{PersistentViewState, RecipeRequestMeta};
use lvu_core::{RecordId, SourceDefinition, SourceId, ViewId};
use lvu_memory::{
    DraftState, NavigationState, PresentationState, RecipeCandidate, RecipeFile, SavedRecipe,
    SourceMetadata, SuggestionOutcome, WorkingView, WorkspaceStore,
};

#[derive(Clone, Debug)]
pub struct SuggestionContext {
    pub source: SourceId,
    pub project: Option<String>,
    pub command: Option<String>,
    pub fields: BTreeMap<String, String>,
}

const QUEUE_CAPACITY: usize = 32;
pub const RECENT_LIMIT: u32 = 32;

#[derive(Clone, Debug)]
pub struct SaveRequest {
    pub sequence: u64,
    pub definition: SourceDefinition,
    pub view_id: ViewId,
    pub state: PersistentViewState,
}
#[derive(Debug)]
enum Command {
    Load(Box<SourceDefinition>, ViewId),
    Save(Box<SaveRequest>),
    Recent,
    ListRecipes(RecipeRequestMeta, Option<SuggestionContext>),
    RecipeHistory(RecipeRequestMeta, lvu_core::RecipeId),
    SaveRecipe(
        RecipeRequestMeta,
        Box<RecipeFile>,
        Option<uuid::Uuid>,
        Option<SuggestionContext>,
    ),
    ImportRecipe(RecipeRequestMeta, PathBuf),
    ExportRecipe(RecipeRequestMeta, lvu_core::RecipeId, uuid::Uuid, PathBuf),
    RecordSuggestion(RecipeOutcome),
    Flush(SyncSender<Result<(), String>>),
    Stop,
}
pub enum Event {
    Loaded(SourceId, ViewId, Vec<WorkingView>),
    LoadFailed(SourceId, ViewId, String),
    Saved(SourceId, ViewId, u64),
    SaveFailed(SourceId, ViewId, u64, String),
    Recent(Vec<SourceMetadata>),
    RecentFailed(String),
    Recipes(
        RecipeRequestMeta,
        Vec<(RecipeFile, String)>,
        Vec<RecipeCandidate>,
    ),
    RecipeHistory(RecipeRequestMeta, Vec<RecipeFile>),
    RecipeSaved(RecipeRequestMeta, SavedRecipe),
    RecipeExported(RecipeRequestMeta, SavedRecipe),
    RecipeFailed(RecipeRequestMeta, String),
    SuggestionFailed(String),
    Fatal(String),
}

pub struct MemoryWorker {
    tx: SyncSender<Command>,
    rx: Receiver<Event>,
    _join: thread::JoinHandle<()>,
}
impl MemoryWorker {
    pub fn start(root: PathBuf) -> Self {
        Self::start_with_capacities(root, QUEUE_CAPACITY, QUEUE_CAPACITY)
    }

    fn start_with_capacities(
        root: PathBuf,
        command_capacity: usize,
        event_capacity: usize,
    ) -> Self {
        let (tx, commands) = mpsc::sync_channel(command_capacity);
        let (events, rx) = mpsc::sync_channel(event_capacity);
        let join = thread::spawn(move || worker(root, commands, events));
        Self {
            tx,
            rx,
            _join: join,
        }
    }
    pub fn load(&self, definition: SourceDefinition, view_id: ViewId) -> Result<(), String> {
        self.tx
            .try_send(Command::Load(Box::new(definition), view_id))
            .map_err(queue_error)
    }
    pub fn save(&self, request: Box<SaveRequest>) -> Result<(), Box<SaveRequest>> {
        match self.tx.try_send(Command::Save(request)) {
            Ok(()) => Ok(()),
            Err(
                TrySendError::Full(Command::Save(value))
                | TrySendError::Disconnected(Command::Save(value)),
            ) => Err(value),
            Err(_) => unreachable!(),
        }
    }
    pub fn recent(&self) -> Result<(), String> {
        self.tx.try_send(Command::Recent).map_err(queue_error)
    }
    pub fn list_recipes(
        &self,
        meta: RecipeRequestMeta,
        context: Option<SuggestionContext>,
    ) -> Result<(), String> {
        self.tx
            .try_send(Command::ListRecipes(meta, context))
            .map_err(queue_error)
    }
    pub fn save_recipe(
        &self,
        meta: RecipeRequestMeta,
        recipe: RecipeFile,
        expected_revision: Option<uuid::Uuid>,
        context: Option<SuggestionContext>,
    ) -> Result<(), String> {
        self.tx
            .try_send(Command::SaveRecipe(
                meta,
                Box::new(recipe),
                expected_revision,
                context,
            ))
            .map_err(queue_error)
    }
    pub fn recipe_history(
        &self,
        meta: RecipeRequestMeta,
        id: lvu_core::RecipeId,
    ) -> Result<(), String> {
        self.tx
            .try_send(Command::RecipeHistory(meta, id))
            .map_err(queue_error)
    }
    pub fn import_recipe(&self, meta: RecipeRequestMeta, path: PathBuf) -> Result<(), String> {
        self.tx
            .try_send(Command::ImportRecipe(meta, path))
            .map_err(queue_error)
    }
    pub fn export_recipe(
        &self,
        meta: RecipeRequestMeta,
        recipe: lvu_core::RecipeId,
        revision: uuid::Uuid,
        path: PathBuf,
    ) -> Result<(), String> {
        self.tx
            .try_send(Command::ExportRecipe(meta, recipe, revision, path))
            .map_err(queue_error)
    }
    pub fn record_suggestion(&self, outcome: RecipeOutcome) -> Result<(), String> {
        self.tx
            .try_send(Command::RecordSuggestion(outcome))
            .map_err(queue_error)
    }
    pub fn poll(&self) -> Option<Event> {
        self.rx.try_recv().ok()
    }
    pub fn flush(&self, timeout: Duration) -> (Vec<Event>, Result<(), String>) {
        let (tx, rx) = mpsc::sync_channel(0);
        let deadline = std::time::Instant::now() + timeout;
        let mut command = Command::Flush(tx);
        let mut events = Vec::with_capacity(QUEUE_CAPACITY * 2);
        loop {
            match self.tx.try_send(command) {
                Ok(()) => break,
                Err(TrySendError::Full(value)) if std::time::Instant::now() < deadline => {
                    command = value;
                    while let Ok(event) = self.rx.try_recv() {
                        events.push(event);
                    }
                    thread::sleep(Duration::from_millis(2));
                }
                Err(_) => return (events, Err("memory worker disconnected".into())),
            }
        }
        loop {
            while let Ok(event) = self.rx.try_recv() {
                events.push(event);
            }
            match rx.try_recv() {
                Ok(result) => return (events, result),
                Err(mpsc::TryRecvError::Disconnected) => {
                    return (
                        events,
                        Err("memory flush acknowledgement disconnected".into()),
                    );
                }
                Err(mpsc::TryRecvError::Empty) if std::time::Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(2));
                }
                Err(mpsc::TryRecvError::Empty) => {
                    return (
                        events,
                        Err("memory autosave flush deadline exceeded".into()),
                    );
                }
            }
        }
    }
    pub fn stop(&self) {
        let _ = self.tx.try_send(Command::Stop);
    }
}
fn queue_error<T>(error: TrySendError<T>) -> String {
    match error {
        TrySendError::Full(_) => "memory worker queue is full".into(),
        TrySendError::Disconnected(_) => "memory worker disconnected".into(),
    }
}

fn worker(root: PathBuf, commands: Receiver<Command>, events: SyncSender<Event>) {
    let mut store = match WorkspaceStore::open(root) {
        Ok(store) => store,
        Err(error) => {
            let _ = events.send(Event::Fatal(format!("memory unavailable: {error}")));
            return;
        }
    };
    let mut versions: HashMap<ViewId, u64> = HashMap::new();
    let mut newest: HashMap<ViewId, u64> = HashMap::new();
    let mut failed: HashMap<ViewId, String> = HashMap::new();
    let mut recipe_failure: Option<String> = None;
    while let Ok(command) = commands.recv() {
        match command {
            Command::Load(definition, view_id) => {
                let definition = *definition;
                let result = (|| {
                    let metadata = source_metadata(definition.clone());
                    store.upsert_source(&metadata)?;
                    let views = store.working_views_for_source(definition.id, 32)?;
                    for value in &views {
                        versions.insert(value.id, value.version);
                    }
                    Ok::<_, lvu_memory::MemoryError>(views)
                })();
                match result {
                    Ok(views) => {
                        if events
                            .send(Event::Loaded(definition.id, view_id, views))
                            .is_err()
                        {
                            break;
                        }
                        match store.recent_sources(None, RECENT_LIMIT) {
                            Ok(values) => {
                                if events.send(Event::Recent(values)).is_err() {
                                    break;
                                }
                            }
                            Err(error) => {
                                if events.send(Event::RecentFailed(error.to_string())).is_err() {
                                    break;
                                }
                            }
                        }
                    }
                    Err(error) => {
                        if events
                            .send(Event::LoadFailed(definition.id, view_id, error.to_string()))
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
            Command::Save(request) => {
                if newest
                    .get(&request.view_id)
                    .is_some_and(|seen| *seen >= request.sequence)
                {
                    if events
                        .send(Event::Saved(
                            request.definition.id,
                            request.view_id,
                            request.sequence,
                        ))
                        .is_err()
                    {
                        break;
                    }
                    continue;
                }
                let expected = versions.get(&request.view_id).copied();
                let metadata = source_metadata(request.definition.clone());
                let view = working_view(&request);
                let result =
                    if request.state.bookmarks.iter().any(|bookmark| {
                        bookmark.id.source_id != request.definition.id.0.to_string()
                    }) {
                        Err(lvu_memory::MemoryError::InvalidData(
                            "bookmark source does not match the working view".into(),
                        ))
                    } else {
                        store.save_source_and_view(&metadata, &view, expected)
                    };
                match result {
                    Ok(version) => {
                        newest.insert(request.view_id, request.sequence);
                        versions.insert(request.view_id, version);
                        failed.remove(&request.view_id);
                        if events
                            .send(Event::Saved(
                                request.definition.id,
                                request.view_id,
                                request.sequence,
                            ))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(error) => {
                        let message = format!("memory autosave: {error}");
                        failed.insert(request.view_id, message.clone());
                        if events
                            .send(Event::SaveFailed(
                                request.definition.id,
                                request.view_id,
                                request.sequence,
                                message,
                            ))
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
            Command::Recent => match store.recent_sources(None, RECENT_LIMIT) {
                Ok(values) => {
                    if events.send(Event::Recent(values)).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    if events
                        .send(Event::RecentFailed(format!("recent sources: {error}")))
                        .is_err()
                    {
                        break;
                    }
                }
            },
            Command::ListRecipes(meta, context) => match store.list_recipes(128) {
                Ok(values) => {
                    let candidates = context.map_or_else(
                        || Ok(Vec::new()),
                        |context| {
                            store.candidates(
                                context.source,
                                context.project.as_deref(),
                                context.command.as_deref(),
                                &context.fields,
                                16,
                            )
                        },
                    );
                    let Ok(candidates) = candidates else {
                        let error = candidates.unwrap_err();
                        if events
                            .send(Event::RecipeFailed(
                                meta,
                                format!("suggest recipes: {error}"),
                            ))
                            .is_err()
                        {
                            break;
                        }
                        continue;
                    };
                    if events
                        .send(Event::Recipes(meta, values, candidates))
                        .is_err()
                    {
                        break;
                    }
                }
                Err(error) => {
                    if events
                        .send(Event::RecipeFailed(meta, format!("list recipes: {error}")))
                        .is_err()
                    {
                        break;
                    }
                }
            },
            Command::RecipeHistory(meta, id) => {
                let event = match store.recipe_revision_documents(id, 100) {
                    Ok(values) => Event::RecipeHistory(meta, values),
                    Err(error) => Event::RecipeFailed(meta, format!("recipe history: {error}")),
                };
                if events.send(event).is_err() {
                    break;
                }
            }
            Command::SaveRecipe(meta, recipe, expected_revision, context) => match (|| {
                if let Some(revision) = expected_revision {
                    return store.update_recipe_revision(recipe.recipe_id, revision, &recipe.view);
                }
                if let Some(context) = context {
                    let mut metadata = source_metadata(recipe.source.clone());
                    metadata.project = context.project;
                    metadata.command = context.command;
                    metadata.fields = context.fields;
                    store.upsert_source(&metadata)?;
                }
                store.save_new_recipe(&recipe)
            })() {
                Ok(saved) => {
                    recipe_failure = None;
                    if events.send(Event::RecipeSaved(meta, saved)).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    let message = format!("save recipe: {error}");
                    recipe_failure = Some(message.clone());
                    if events.send(Event::RecipeFailed(meta, message)).is_err() {
                        break;
                    }
                }
            },
            Command::ImportRecipe(meta, path) => match store.import_new_recipe(&path) {
                Ok(saved) => {
                    recipe_failure = None;
                    if events.send(Event::RecipeSaved(meta, saved)).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    let message = format!("import recipe: {error}");
                    recipe_failure = Some(message.clone());
                    if events.send(Event::RecipeFailed(meta, message)).is_err() {
                        break;
                    }
                }
            },
            Command::ExportRecipe(meta, recipe, revision, path) => {
                let event = match store.export_recipe_revision(recipe, revision, &path) {
                    Ok(saved) => Event::RecipeExported(meta, saved),
                    Err(error) => Event::RecipeFailed(meta, format!("export recipe: {error}")),
                };
                if events.send(event).is_err() {
                    break;
                }
            }
            Command::RecordSuggestion(outcome) => {
                let result = (|| {
                    let source =
                        SourceId(uuid::Uuid::parse_str(&outcome.source_id).map_err(|error| {
                            lvu_memory::MemoryError::InvalidData(error.to_string())
                        })?);
                    let recipe =
                        lvu_core::RecipeId(uuid::Uuid::parse_str(&outcome.recipe_id).map_err(
                            |error| lvu_memory::MemoryError::InvalidData(error.to_string()),
                        )?);
                    let revision = uuid::Uuid::parse_str(&outcome.revision)
                        .map_err(|error| lvu_memory::MemoryError::InvalidData(error.to_string()))?;
                    let now = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs()
                        .min(i64::MAX as u64) as i64;
                    let kind = if outcome.accepted {
                        SuggestionOutcome::Accepted
                    } else {
                        SuggestionOutcome::Rejected
                    };
                    store.record_suggestion(source, recipe, revision, kind, now)?;
                    if outcome.accepted {
                        store.record_usage(source, recipe, now)?;
                    }
                    Ok::<(), lvu_memory::MemoryError>(())
                })();
                if let Err(error) = result
                    && events
                        .send(Event::SuggestionFailed(format!(
                            "record recipe suggestion: {error}"
                        )))
                        .is_err()
                {
                    break;
                }
            }
            Command::Flush(done) => {
                let result = if let Some(error) = &recipe_failure {
                    Err(error.clone())
                } else if failed.is_empty() {
                    Ok(())
                } else {
                    Err(failed.values().next().expect("nonempty").clone())
                };
                let _ = done.send(result);
            }
            Command::Stop => break,
        }
    }
}
fn source_metadata(definition: SourceDefinition) -> SourceMetadata {
    let command = match &definition.acquisition {
        lvu_core::Acquisition::File { path, .. } => Some(path.to_string_lossy().into_owned()),
        lvu_core::Acquisition::Command { command } => Some(format!("{command:?}")),
        lvu_core::Acquisition::Http { url, .. } => Some(url.clone()),
        lvu_core::Acquisition::Stdin => Some("standard input".into()),
    };
    SourceMetadata {
        definition,
        project: None,
        command,
        fields: BTreeMap::new(),
        last_seen: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .min(i64::MAX as u64) as i64,
        missing: false,
    }
}
fn working_view(request: &SaveRequest) -> WorkingView {
    let selected = request.state.selected.as_ref().and_then(|row| {
        SourceId(uuid::Uuid::parse_str(&row.source_id).ok()?)
            .eq(&request.definition.id)
            .then_some(RecordId {
                source_id: request.definition.id,
                sequence: row.sequence,
            })
    });
    WorkingView {
        id: request.view_id,
        source_id: request.definition.id,
        name: if request.state.view_name.is_empty() {
            "Raw events".into()
        } else {
            request.state.view_name.clone()
        },
        applied_revision_id: None,
        applied_search: request.state.applied_search.clone(),
        search_draft: Some(request.state.search_draft.clone()),
        applied_advanced_filter: nonempty(&request.state.applied_advanced),
        advanced_filter_draft: Some(DraftState {
            text: request.state.advanced_draft.clone(),
            diagnostics: request.state.advanced_error.clone().into_iter().collect(),
        }),
        navigation: NavigationState {
            selected,
            anchor: None,
            follow: request.state.follow,
        },
        presentation: PresentationState {
            bookmarks: request
                .state
                .bookmarks
                .iter()
                .filter_map(|bookmark| {
                    Some(lvu_memory::StoredBookmark {
                        record: RecordId {
                            source_id: SourceId(
                                uuid::Uuid::parse_str(&bookmark.id.source_id).ok()?,
                            ),
                            sequence: bookmark.id.sequence,
                        },
                        note: bookmark.note.clone(),
                    })
                })
                .collect(),
            pinned_columns: request.state.pinned_columns.clone(),
            color_field: request.state.color_field.clone(),
            applied_enrichment: request
                .state
                .applied_enrichments
                .last()
                .map(|stage| stage.source.clone()),
            enrichment_chain: Some(
                request
                    .state
                    .applied_enrichments
                    .iter()
                    .map(|stage| lvu_memory::StoredEnrichment {
                        id: stage.id.0.clone(),
                        source: stage.source.clone(),
                    })
                    .collect(),
            ),
            enrichment_editing: request
                .state
                .enrichment_editing
                .as_ref()
                .map(|id| id.0.clone()),
            enrichment_selected: request
                .state
                .applied_enrichments
                .get(request.state.enrichment_selected)
                .map(|stage| stage.id.0.clone()),
            enrichment_draft: Some(DraftState {
                text: request.state.enrichment_draft.clone(),
                diagnostics: request.state.enrichment_error.clone().into_iter().collect(),
            }),
            applied_grouping: nonempty(&request.state.applied_grouping),
            grouping_draft: Some(DraftState {
                text: request.state.grouping_draft.clone(),
                diagnostics: request.state.grouping_error.clone().into_iter().collect(),
            }),
            capture_time: request.state.applied_capture_time_policy.map_or_else(
                || {
                    request.state.applied_capture_time.map(|window| {
                        lvu_memory::TimePolicy::Absolute {
                            start_unix_nanos: window.start_unix_nanos,
                            end_unix_nanos: window.end_unix_nanos,
                        }
                    })
                },
                |policy| {
                    Some(match policy {
                        lvu::CaptureTimePolicy::Absolute(window) => {
                            lvu_memory::TimePolicy::Absolute {
                                start_unix_nanos: window.start_unix_nanos,
                                end_unix_nanos: window.end_unix_nanos,
                            }
                        }
                        lvu::CaptureTimePolicy::Recent { seconds } => {
                            lvu_memory::TimePolicy::Recent { seconds }
                        }
                    })
                },
            ),
            time_basis: match request.state.applied_time_basis {
                lvu::TimeBasis::Capture => lvu_memory::TimeBasis::Capture,
                lvu::TimeBasis::Event => lvu_memory::TimeBasis::Event,
                lvu::TimeBasis::Extracted => lvu_memory::TimeBasis::Extracted,
            },
            capture_time_start_draft: request.state.time_start_draft.clone(),
            capture_time_end_draft: request.state.time_end_draft.clone(),
            capture_time_error: request.state.time_error.clone(),
        },
        version: 0,
    }
}
fn nonempty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_owned())
}

pub fn restored(value: WorkingView) -> PersistentViewState {
    let applied_enrichments: Vec<_> = value
        .presentation
        .effective_enrichments()
        .into_iter()
        .map(|stage| lvu::EnrichmentDefinition {
            id: lvu::EnrichmentStageId(stage.id),
            source: stage.source,
        })
        .collect();
    let enrichment_selected = value
        .presentation
        .enrichment_selected
        .as_ref()
        .and_then(|id| {
            applied_enrichments
                .iter()
                .position(|stage| &stage.id.0 == id)
        })
        .unwrap_or(0);
    let enrichment_editing = value
        .presentation
        .enrichment_editing
        .clone()
        .filter(|id| applied_enrichments.iter().any(|stage| &stage.id.0 == id))
        .map(lvu::EnrichmentStageId);
    let stored_capture_time = value.presentation.capture_time.clone();
    let (applied_capture_time, applied_capture_time_policy) = match stored_capture_time {
        Some(lvu_memory::TimePolicy::Absolute {
            start_unix_nanos,
            end_unix_nanos,
        }) => {
            let window = lvu::CaptureTimeRange {
                start_unix_nanos,
                end_unix_nanos,
            };
            (Some(window), Some(lvu::CaptureTimePolicy::Absolute(window)))
        }
        Some(lvu_memory::TimePolicy::Recent { seconds }) => {
            (None, Some(lvu::CaptureTimePolicy::Recent { seconds }))
        }
        _ => (None, None),
    };
    PersistentViewState {
        bookmarks: value
            .presentation
            .bookmarks
            .iter()
            .map(|bookmark| lvu::Bookmark {
                id: lvu::RowId::new(
                    bookmark.record.source_id.0.to_string(),
                    bookmark.record.sequence,
                ),
                note: bookmark.note.clone(),
            })
            .collect(),
        view_name: value.name,
        applied_search: value.applied_search,
        search_draft: value.search_draft.unwrap_or_default(),
        search_error: None,
        applied_advanced: value.applied_advanced_filter.unwrap_or_default(),
        advanced_draft: value
            .advanced_filter_draft
            .as_ref()
            .map_or_else(String::new, |draft| draft.text.clone()),
        advanced_error: value
            .advanced_filter_draft
            .and_then(|draft| draft.diagnostics.into_iter().next()),
        selected: value
            .navigation
            .selected
            .map(|id| lvu::RowId::new(id.source_id.0.to_string(), id.sequence)),
        follow: value.navigation.follow,
        pinned_columns: value.presentation.pinned_columns,
        color_field: value.presentation.color_field,
        applied_enrichment: applied_enrichments
            .last()
            .map_or_else(String::new, |stage| stage.source.clone()),
        applied_enrichments,
        enrichment_selected,
        enrichment_editing,
        enrichment_draft: value
            .presentation
            .enrichment_draft
            .as_ref()
            .map_or_else(String::new, |draft| draft.text.clone()),
        enrichment_error: value
            .presentation
            .enrichment_draft
            .and_then(|draft| draft.diagnostics.into_iter().next()),
        applied_grouping: value.presentation.applied_grouping.unwrap_or_default(),
        grouping_draft: value
            .presentation
            .grouping_draft
            .as_ref()
            .map_or_else(String::new, |draft| draft.text.clone()),
        grouping_error: value
            .presentation
            .grouping_draft
            .and_then(|draft| draft.diagnostics.into_iter().next()),
        applied_capture_time,
        applied_capture_time_policy,
        applied_time_basis: match value.presentation.time_basis {
            lvu_memory::TimeBasis::Capture => lvu::TimeBasis::Capture,
            lvu_memory::TimeBasis::Event => lvu::TimeBasis::Event,
            lvu_memory::TimeBasis::Extracted => lvu::TimeBasis::Extracted,
        },
        time_start_draft: value.presentation.capture_time_start_draft,
        time_end_draft: value.presentation.capture_time_end_draft,
        time_recent_draft: match value.presentation.capture_time {
            Some(lvu_memory::TimePolicy::Recent { seconds }) => {
                lvu::format_capture_duration(seconds)
            }
            _ => String::new(),
        },
        time_error: value.presentation.capture_time_error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lvu_core::{Acquisition, SourceDefinition};
    use std::time::Instant;
    use tempfile::TempDir;

    fn definition() -> SourceDefinition {
        SourceDefinition {
            schema_version: 1,
            id: SourceId::new(),
            name: "remembered".into(),
            acquisition: Acquisition::File {
                path: "/tmp/example.log".into(),
                follow: true,
            },
            identity_hints: BTreeMap::new(),
            retention: None,
        }
    }
    fn request(
        sequence: u64,
        definition: SourceDefinition,
        view_id: ViewId,
        search: &str,
    ) -> SaveRequest {
        SaveRequest {
            sequence,
            definition,
            view_id,
            state: PersistentViewState {
                applied_search: search.into(),
                search_draft: search.into(),
                follow: true,
                ..PersistentViewState::default()
            },
        }
    }

    #[test]
    fn full_slow_worker_queue_never_blocks_the_caller() {
        let (tx, commands) = mpsc::sync_channel(QUEUE_CAPACITY);
        for _ in 0..QUEUE_CAPACITY {
            tx.try_send(Command::Recent).unwrap();
        }
        let (events, rx) = mpsc::sync_channel(1);
        let join = thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            drop(commands);
            drop(events);
        });
        let worker = MemoryWorker {
            tx,
            rx,
            _join: join,
        };
        let definition = definition();
        let start = Instant::now();
        assert!(
            worker
                .save(Box::new(request(1, definition, ViewId::new(), "latest")))
                .is_err()
        );
        assert!(start.elapsed() < Duration::from_millis(20));
    }

    #[test]
    fn stale_save_sequence_cannot_regress_latest_state() {
        let temp = TempDir::new().unwrap();
        let worker = MemoryWorker::start(temp.path().to_path_buf());
        let definition = definition();
        let id = ViewId::new();
        worker
            .save(Box::new(request(2, definition.clone(), id, "latest")))
            .unwrap();
        worker
            .save(Box::new(request(1, definition.clone(), id, "stale")))
            .unwrap();
        assert!(worker.flush(Duration::from_secs(1)).1.is_ok());
        let store = WorkspaceStore::open(temp.path()).unwrap();
        assert_eq!(
            store.get_view(id).unwrap().unwrap().applied_search,
            "latest"
        );
        worker.stop();
    }

    #[test]
    fn load_completion_is_not_dropped_when_event_queue_is_full() {
        let temp = TempDir::new().unwrap();
        let (commands_tx, commands_rx) = mpsc::sync_channel(0);
        let (events_tx, events_rx) = mpsc::sync_channel(0);
        let root = temp.path().to_path_buf();
        let join = thread::spawn(move || worker(root, commands_rx, events_tx));

        // The completed command rendezvous proves the worker received Recent.
        // It then blocks on the zero-capacity event rendezvous while a scoped
        // sender waits to hand over Load; neither result can be dropped.
        commands_tx.send(Command::Recent).unwrap();
        let definition = definition();
        let source_id = definition.id;
        let view_id = ViewId::new();
        let load_tx = commands_tx.clone();
        let load_sender = thread::spawn(move || {
            load_tx
                .send(Command::Load(Box::new(definition), view_id))
                .unwrap();
        });
        assert!(matches!(
            events_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            Event::Recent(_)
        ));
        let Event::Loaded(id, requested, _) =
            events_rx.recv_timeout(Duration::from_secs(2)).unwrap()
        else {
            panic!("expected reliable load completion");
        };
        load_sender.join().unwrap();
        assert_eq!(id, source_id);
        assert_eq!(requested, view_id);
        assert!(matches!(
            events_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            Event::Recent(_)
        ));
        commands_tx.send(Command::Stop).unwrap();
        join.join().unwrap();
    }

    #[test]
    fn failed_save_is_acknowledged_and_makes_flush_fail() {
        let temp = TempDir::new().unwrap();
        let worker = MemoryWorker::start(temp.path().to_path_buf());
        let definition = definition();
        let view_id = ViewId::new();
        worker
            .save(Box::new(request(1, definition.clone(), view_id, "first")))
            .unwrap();
        assert!(worker.flush(Duration::from_secs(1)).1.is_ok());

        let external = WorkspaceStore::open(temp.path()).unwrap();
        let mut view = external.get_view(view_id).unwrap().unwrap();
        view.applied_search = "external".into();
        external.update_view(&view, 0).unwrap();
        worker
            .save(Box::new(request(2, definition, view_id, "unsaved")))
            .unwrap();
        let (events, result) = worker.flush(Duration::from_secs(1));
        assert!(result.is_err());
        assert!(events.iter().any(|event| matches!(
            event,
            Event::SaveFailed(_, id, 2, message)
                if *id == view_id && message.contains("conflict")
        )));
        assert_eq!(
            external.get_view(view_id).unwrap().unwrap().applied_search,
            "external"
        );
        worker.stop();
    }

    #[test]
    fn chain_roundtrip_keeps_step_ids_selection_and_clear_without_legacy_resurrection() {
        let mut value = request(1, definition(), ViewId::new(), "");
        let stages = vec![
            lvu::EnrichmentDefinition {
                id: lvu::EnrichmentStageId("first".into()),
                source: r"/id=(?P<id>\w+)/".into(),
            },
            lvu::EnrichmentDefinition {
                id: lvu::EnrichmentStageId("second".into()),
                source: "upper = pl.col('id').str.to_uppercase()".into(),
            },
        ];
        value.state.applied_enrichments = stages.clone();
        value.state.enrichment_selected = 1;
        value.state.enrichment_editing = Some(stages[1].id.clone());
        value.state.enrichment_draft = "upper = pl.col(".into();
        let loaded = restored(working_view(&value));
        assert_eq!(loaded.applied_enrichments, stages);
        assert_eq!(loaded.enrichment_selected, 1);
        assert_eq!(loaded.enrichment_editing, Some(stages[1].id.clone()));
        assert_eq!(loaded.enrichment_draft, "upper = pl.col(");

        value.state.applied_enrichments.clear();
        value.state.applied_enrichment = "stale = pl.lit(1)".into();
        let cleared = restored(working_view(&value));
        assert!(cleared.applied_enrichments.is_empty());
        assert!(cleared.applied_enrichment.is_empty());
        assert!(cleared.enrichment_editing.is_none());
    }

    #[test]
    fn autosave_keeps_accepted_filter_separate_from_unfinished_draft() {
        let temp = TempDir::new().unwrap();
        let worker = MemoryWorker::start(temp.path().to_path_buf());
        let definition = definition();
        let view_id = ViewId::new();
        let mut value = request(1, definition, view_id, "accepted");
        value.state.search_draft = "new draft while query pending".into();
        value.state.applied_advanced = "pl.col('raw').is_not_null()".into();
        value.state.advanced_draft = "pl.col(".into();
        value.state.pinned_columns = vec!["service".into()];
        value.state.color_field = Some("request_id".into());
        value.state.applied_enrichment = "status = pl.lit(200)".into();
        value.state.applied_enrichments = vec![lvu::EnrichmentDefinition {
            id: lvu::EnrichmentStageId("status-step".into()),
            source: value.state.applied_enrichment.clone(),
        }];
        value.state.enrichment_editing = Some(lvu::EnrichmentStageId("status-step".into()));
        value.state.enrichment_draft = "status = pl.col(".into();
        value.state.enrichment_error = Some("unfinished".into());
        value.state.applied_grouping = r"^(\s+|Caused by:)".into();
        value.state.grouping_draft = r"^\s+|Caused by:".into();
        value.state.grouping_error = Some("unfinished grouping edit".into());
        value.state.applied_capture_time = Some(lvu::CaptureTimeRange {
            start_unix_nanos: 10,
            end_unix_nanos: 20,
        });
        value.state.applied_time_basis = lvu::TimeBasis::Extracted;
        value.state.time_start_draft = "unfinished start".into();
        value.state.time_error = Some("invalid UTC".into());
        worker.save(Box::new(value)).unwrap();
        assert!(worker.flush(Duration::from_secs(1)).1.is_ok());

        let stored = WorkspaceStore::open(temp.path())
            .unwrap()
            .get_view(view_id)
            .unwrap()
            .unwrap();
        assert_eq!(stored.applied_search, "accepted");
        assert_eq!(
            stored.search_draft.as_deref(),
            Some("new draft while query pending")
        );
        assert_eq!(
            stored.applied_advanced_filter.as_deref(),
            Some("pl.col('raw').is_not_null()")
        );
        assert_eq!(
            stored.advanced_filter_draft.as_ref().unwrap().text,
            "pl.col("
        );
        assert_eq!(stored.presentation.pinned_columns, ["service"]);
        assert_eq!(
            stored.presentation.color_field.as_deref(),
            Some("request_id")
        );
        assert_eq!(
            stored.presentation.applied_enrichment.as_deref(),
            Some("status = pl.lit(200)")
        );
        assert_eq!(
            stored.presentation.enrichment_draft.as_ref().unwrap().text,
            "status = pl.col("
        );
        assert_eq!(
            stored.presentation.applied_grouping.as_deref(),
            Some(r"^(\s+|Caused by:)")
        );
        assert_eq!(
            stored.presentation.grouping_draft.as_ref().unwrap().text,
            r"^\s+|Caused by:"
        );
        assert_eq!(
            stored
                .presentation
                .grouping_draft
                .as_ref()
                .unwrap()
                .diagnostics,
            ["unfinished grouping edit"]
        );
        assert_eq!(
            stored.presentation.capture_time,
            Some(lvu_memory::TimePolicy::Absolute {
                start_unix_nanos: 10,
                end_unix_nanos: 20,
            })
        );
        assert_eq!(
            stored.presentation.time_basis,
            lvu_memory::TimeBasis::Extracted
        );
        assert_eq!(
            stored.presentation.capture_time_start_draft,
            "unfinished start"
        );
        let restored = super::restored(stored);
        assert_eq!(restored.applied_grouping, r"^(\s+|Caused by:)");
        assert_eq!(restored.grouping_draft, r"^\s+|Caused by:");
        assert_eq!(
            restored.grouping_error.as_deref(),
            Some("unfinished grouping edit")
        );
        worker.stop();
    }

    #[test]
    fn rolling_capture_policy_round_trips_without_persisting_resolved_bounds() {
        let temp = TempDir::new().unwrap();
        let worker = MemoryWorker::start(temp.path().to_path_buf());
        let definition = definition();
        let view_id = ViewId::new();
        let mut value = request(1, definition, view_id, "accepted");
        value.state.applied_capture_time = Some(lvu::CaptureTimeRange {
            start_unix_nanos: 1_000,
            end_unix_nanos: 2_000,
        });
        value.state.applied_capture_time_policy =
            Some(lvu::CaptureTimePolicy::Recent { seconds: 30 });
        value.state.time_recent_draft = "30s".into();
        worker.save(Box::new(value)).unwrap();
        assert!(worker.flush(Duration::from_secs(1)).1.is_ok());
        worker.stop();

        let stored = WorkspaceStore::open(temp.path())
            .unwrap()
            .get_view(view_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            stored.presentation.capture_time,
            Some(lvu_memory::TimePolicy::Recent { seconds: 30 })
        );
        let restored = super::restored(stored);
        assert_eq!(restored.applied_capture_time, None);
        assert_eq!(
            restored.applied_capture_time_policy,
            Some(lvu::CaptureTimePolicy::Recent { seconds: 30 })
        );
        assert_eq!(restored.time_recent_draft, "30s");
    }

    #[test]
    fn corrupt_store_reports_error_without_blocking_commands() {
        let temp = TempDir::new().unwrap();
        std::fs::write(temp.path().join("workspace.sqlite3"), b"broken sqlite").unwrap();
        let worker = MemoryWorker::start(temp.path().to_path_buf());
        let start = Instant::now();
        let _ = worker.recent();
        assert!(start.elapsed() < Duration::from_millis(20));
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if let Some(Event::Fatal(message)) = worker.poll() {
                assert!(message.contains("memory unavailable"));
                break;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(5));
        }
    }
}

#[cfg(test)]
mod bookmark_tests {
    use super::*;
    #[test]
    fn bookmark_ids_and_notes_survive_durable_reopen_and_invalid_sources_are_refused() {
        let root = tempfile::TempDir::new().unwrap();
        let definition = SourceDefinition {
            schema_version: 1,
            id: SourceId::new(),
            name: "bookmarks".into(),
            acquisition: lvu_core::Acquisition::File {
                path: root.path().join("file.log"),
                follow: true,
            },
            identity_hints: BTreeMap::new(),
            retention: None,
        };
        let id = ViewId::new();
        let mut request = SaveRequest {
            sequence: 1,
            definition: definition.clone(),
            view_id: id,
            state: PersistentViewState {
                view_name: "notes".into(),
                bookmarks: vec![lvu::Bookmark {
                    id: lvu::RowId::new(definition.id.0.to_string(), 42),
                    note: "Café retry".into(),
                }],
                ..Default::default()
            },
        };
        let mut store = WorkspaceStore::open(root.path()).unwrap();
        store
            .save_source_and_view(
                &source_metadata(definition.clone()),
                &working_view(&request),
                None,
            )
            .unwrap();
        drop(store);
        let store = WorkspaceStore::open(root.path()).unwrap();
        assert_eq!(
            restored(store.get_view(id).unwrap().unwrap()).bookmarks,
            request.state.bookmarks
        );
        request.state.bookmarks[0].id.source_id = SourceId::new().0.to_string();
        assert!(store.update_view(&working_view(&request), 0).is_err());
        request.state.bookmarks[0].id.source_id = definition.id.0.to_string();
        request.state.bookmarks[0].note = "x".repeat(1025);
        assert!(store.update_view(&working_view(&request), 0).is_err());
    }
}
