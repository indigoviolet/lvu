use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
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

/// Namespace for every deterministic identity lvu derives for a source.
pub const SOURCE_NAMESPACE: uuid::Uuid = uuid::Uuid::from_bytes([
    0x6c, 0x76, 0x75, 0x00, 0x73, 0x6f, 0x75, 0x72, 0x63, 0x65, 0x00, 0x6e, 0x73, 0x00, 0x00, 0x01,
]);

/// Preferred identity for a source's canonical view.
///
/// Deterministic so a restart proposes the same identity, but only ever used
/// for a view that does not exist yet: an occupied identity is never adopted.
pub fn canonical_view_id(source_id: SourceId) -> ViewId {
    ViewId(uuid::Uuid::new_v5(
        &SOURCE_NAMESPACE,
        format!("all-events-view:{}", source_id.0).as_bytes(),
    ))
}

/// Display name of every source's canonical view.
pub const CANONICAL_VIEW_NAME: &str = "All events";

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
    CreateDerivedView(Box<SaveRequest>),
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
    /// A derived view was persisted, or could not be. Only the success case
    /// may make the view visible.
    DerivedViewCreated(ViewId, Result<(), String>),
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
    phase: Arc<AtomicU8>,
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
        let phase = Arc::new(AtomicU8::new(0));
        let worker_phase = Arc::clone(&phase);
        let join = thread::spawn(move || worker(root, commands, events, worker_phase));
        Self {
            tx,
            rx,
            _join: join,
            phase,
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
    /// Persists a derived view before it is shown. The reply decides whether
    /// the view is installed at all.
    pub fn create_derived_view(&self, request: Box<SaveRequest>) -> Result<(), String> {
        self.tx
            .try_send(Command::CreateDerivedView(request))
            .map_err(queue_error)
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

    /// A bounded diagnostic snapshot, never a completion or durability signal.
    pub fn phase(&self) -> &'static str {
        match self.phase.load(Ordering::Relaxed) {
            0 => "opening workspace",
            1 => "waiting for command",
            2 => "loading workspace state",
            3 => "persisting view",
            4 => "delivering save result",
            5 => "processing workspace command",
            6 => "acknowledging flush",
            _ => "stopped",
        }
    }

    fn flush_timeout(&self, stage: &str) -> String {
        format!(
            "memory autosave flush deadline exceeded ({stage}; worker: {})",
            self.phase()
        )
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
                Err(TrySendError::Full(_)) => {
                    return (events, Err(self.flush_timeout("waiting to enqueue flush")));
                }
                Err(TrySendError::Disconnected(_)) => {
                    return (events, Err("memory worker disconnected".into()));
                }
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
                        Err(self.flush_timeout("waiting for flush acknowledgement")),
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

fn worker(
    root: PathBuf,
    commands: Receiver<Command>,
    events: SyncSender<Event>,
    phase: Arc<AtomicU8>,
) {
    let mut store = match WorkspaceStore::open(root) {
        Ok(store) => store,
        Err(error) => {
            let _ = events.send(Event::Fatal(format!("memory unavailable: {error}")));
            phase.store(7, Ordering::Relaxed);
            return;
        }
    };
    let mut versions: HashMap<ViewId, u64> = HashMap::new();
    let mut newest: HashMap<ViewId, u64> = HashMap::new();
    let mut failed: HashMap<ViewId, String> = HashMap::new();
    let mut recipe_failure: Option<String> = None;
    phase.store(1, Ordering::Relaxed);
    while let Ok(command) = commands.recv() {
        phase.store(
            match &command {
                Command::Load(..) => 2,
                Command::Save(..) => 3,
                Command::Flush(..) => 6,
                Command::Stop => 7,
                _ => 5,
            },
            Ordering::Relaxed,
        );
        match command {
            Command::Load(definition, view_id) => {
                let definition = *definition;
                let result = (|| {
                    let metadata = source_metadata(definition.clone());
                    store.upsert_source(&metadata)?;
                    // Every source gets its canonical view here, on the way in.
                    // Existing views are loaded untouched and keep their own
                    // identities, names and definitions; the canonical view is
                    // an addition, never a reinterpretation of one of them.
                    let canonical = store.ensure_canonical_view(
                        definition.id,
                        canonical_view_id(definition.id),
                        CANONICAL_VIEW_NAME,
                    )?;
                    let mut views = store.working_views_for_source(definition.id, 33)?;
                    if !views.iter().any(|value| value.id == canonical.id) {
                        views.push(canonical);
                    }
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
            Command::CreateDerivedView(request) => {
                let view = working_view(&request);
                let result = store
                    .create_view(&view)
                    .map(|()| {
                        versions.insert(request.view_id, 0);
                    })
                    .map_err(|error| error.to_string());
                if events
                    .send(Event::DerivedViewCreated(request.view_id, result))
                    .is_err()
                {
                    break;
                }
            }
            Command::Save(request) => {
                if newest
                    .get(&request.view_id)
                    .is_some_and(|seen| *seen >= request.sequence)
                {
                    phase.store(4, Ordering::Relaxed);
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
                    phase.store(1, Ordering::Relaxed);
                    continue;
                }
                let expected = versions.get(&request.view_id).copied();
                let metadata = source_metadata(request.definition.clone());
                let view = working_view(&request);
                let result = if request.state.bookmarks.iter().any(|bookmark| {
                    bookmark.id.source_id != request.definition.id.0.to_string()
                        && !request.state.source_ids.contains(&bookmark.id.source_id)
                }) || request
                    .state
                    .source_ids
                    .iter()
                    .any(|id| uuid::Uuid::parse_str(id).is_err())
                {
                    Err(lvu_memory::MemoryError::InvalidData(
                        "bookmark source does not match the working view".into(),
                    ))
                } else {
                    store.save_source_and_view(&metadata, &view, expected)
                };
                phase.store(4, Ordering::Relaxed);
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
                    // The stamp the caller put on the document it built, so
                    // Save and Update date a revision the same way.
                    return store.update_recipe_revision(
                        recipe.recipe_id,
                        revision,
                        &recipe.view,
                        recipe.saved_at_unix_nanos,
                    );
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
        phase.store(1, Ordering::Relaxed);
    }
    phase.store(7, Ordering::Relaxed);
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
        let source_id = SourceId(uuid::Uuid::parse_str(&row.source_id).ok()?);
        (source_id == request.definition.id || request.state.source_ids.contains(&row.source_id))
            .then_some(RecordId {
                source_id,
                sequence: row.sequence,
            })
    });
    WorkingView {
        id: request.view_id,
        source_id: request.definition.id,
        // Saves never carry a role: the store writes it once when the row is
        // created and ignores it afterwards, so autosave cannot promote or
        // demote a view.
        role: lvu_memory::ViewRole::Derived,
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
            selected_at: request.state.selected_at,
            // The single command slot is migrated on read and never written.
            command_enrichment: None,
            command_enrichment_revision: 0,
            command_publication: None,
            command_steps: request
                .state
                .command_steps
                .iter()
                .map(|(stage, run)| {
                    (
                        stage.clone(),
                        lvu_memory::StoredCommandStep {
                            revision: run.revision,
                            publication: run.publication.clone(),
                        },
                    )
                })
                .collect(),
            source_ids: request
                .state
                .source_ids
                .iter()
                .filter_map(|id| uuid::Uuid::parse_str(id).ok().map(SourceId))
                .collect(),
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
            color_rules: request
                .state
                .color_rules
                .iter()
                .take(lvu::MAX_COLOR_RULES)
                .map(|rule| lvu_memory::StoredColorRule {
                    predicate: rule.predicate.clone(),
                    color: rule.color.label().into(),
                })
                .collect(),
            fold_enabled: request.state.fold_enabled,
            // Zero means "the built-in minimum"; it is not a stored policy.
            fold_minimum_run: (request.state.fold_minimum_run >= 2)
                .then(|| u32::try_from(request.state.fold_minimum_run).unwrap_or(u32::MAX)),
            fold_key_column: request
                .state
                .fold_key_column
                .clone()
                .filter(|column| !column.trim().is_empty()),
            fold_lookback: u32::try_from(request.state.fold_lookback).unwrap_or(u32::MAX),
            fold_normalisation: request.state.fold_normalisation.token().to_owned(),
            fold_expanded: request
                .state
                .fold_expanded
                .iter()
                .filter_map(|id| {
                    Some(RecordId {
                        source_id: SourceId(uuid::Uuid::parse_str(&id.source_id).ok()?),
                        sequence: id.sequence,
                    })
                })
                .take(256)
                .collect(),
            exact_field: request.state.exact_field.clone(),
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
                        command: stage.command.clone(),
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
                lvu::TimeBasis::Selected => lvu_memory::TimeBasis::Selected,
            },
            time_field: request.state.applied_time_field.clone(),
            capture_time_start_draft: request.state.time_start_draft.clone(),
            capture_time_end_draft: request.state.time_end_draft.clone(),
            capture_time_error: request.state.time_error.clone(),
            time_gap_threshold_seconds: request.state.time_gap_threshold_seconds,
            time_draft: request.state.time_structured_draft_present.then(|| {
                lvu_memory::StoredTimeDraft {
                    basis: match request.state.time_basis_draft {
                        lvu::TimeBasis::Capture => lvu_memory::TimeBasis::Capture,
                        lvu::TimeBasis::Event => lvu_memory::TimeBasis::Event,
                        lvu::TimeBasis::Extracted => lvu_memory::TimeBasis::Extracted,
                        lvu::TimeBasis::Selected => lvu_memory::TimeBasis::Selected,
                    },
                    field: request.state.time_field_draft.clone(),
                    window: match request.state.time_window_draft {
                        lvu::app::TimeWindowChoice::All => lvu_memory::StoredTimeWindow::All,
                        lvu::app::TimeWindowChoice::Absolute => {
                            lvu_memory::StoredTimeWindow::Absolute
                        }
                        lvu::app::TimeWindowChoice::Recent(seconds) => {
                            lvu_memory::StoredTimeWindow::Recent { seconds }
                        }
                        lvu::app::TimeWindowChoice::DataFirstToLast => {
                            lvu_memory::StoredTimeWindow::DataFirstToLast
                        }
                        lvu::app::TimeWindowChoice::DataRecent(seconds) => {
                            lvu_memory::StoredTimeWindow::DataRecent { seconds }
                        }
                        lvu::app::TimeWindowChoice::AroundSelected(seconds) => {
                            lvu_memory::StoredTimeWindow::AroundSelected { seconds }
                        }
                    },
                    touched: request.state.time_draft_touched,
                    structured_present: true,
                    start_date: request.state.time_start_date_draft.clone(),
                    start_time: request.state.time_start_clock_draft.clone(),
                    start_zone: request.state.time_start_zone_draft.clone(),
                    end_date: request.state.time_end_date_draft.clone(),
                    end_time: request.state.time_end_clock_draft.clone(),
                    end_zone: request.state.time_end_zone_draft.clone(),
                }
            }),
        },
        version: 0,
    }
}
fn nonempty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_owned())
}

pub fn restored(value: WorkingView) -> PersistentViewState {
    let mut applied_enrichments: Vec<_> = value
        .presentation
        .effective_enrichments()
        .into_iter()
        .map(|stage| lvu::EnrichmentDefinition {
            id: lvu::EnrichmentStageId(stage.id),
            source: stage.source,
            command: stage.command,
        })
        .collect();
    let mut command_steps: std::collections::BTreeMap<String, lvu::app::CommandStepState> = value
        .presentation
        .command_steps
        .iter()
        .map(|(stage, run)| {
            (
                stage.clone(),
                lvu::app::CommandStepState {
                    revision: run.revision,
                    publication: run.publication.clone(),
                },
            )
        })
        .collect();
    // A view saved before command steps joined the chain kept one command
    // in a slot of its own, run after every expression step. It becomes the
    // last step of the chain, named `command` as its results always were,
    // with its revision and last publication intact.
    if let Some(legacy) = value.presentation.command_enrichment.clone()
        && !applied_enrichments.iter().any(|stage| stage.is_command())
    {
        let id = if applied_enrichments
            .iter()
            .any(|stage| stage.id.0 == legacy.id)
        {
            format!("{}-command", legacy.id)
        } else {
            legacy.id
        };
        let mut name = lvu::app::DEFAULT_COMMAND_STEP_NAME.to_owned();
        if !lvu::app::valid_command_step_name(&name) {
            name = "command".into();
        }
        command_steps.insert(
            id.clone(),
            lvu::app::CommandStepState {
                revision: value.presentation.command_enrichment_revision,
                publication: value.presentation.command_publication.clone(),
            },
        );
        applied_enrichments.push(lvu::EnrichmentDefinition::command(
            id,
            name,
            legacy.definition,
        ));
    }
    command_steps.retain(|stage, _| {
        applied_enrichments
            .iter()
            .any(|step| step.is_command() && step.id.0 == *stage)
    });
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
    let accepted_field = value.presentation.time_field.clone();
    let accepted_basis = match value.presentation.time_basis {
        lvu_memory::TimeBasis::Capture => lvu::TimeBasis::Capture,
        lvu_memory::TimeBasis::Event => lvu::TimeBasis::Event,
        lvu_memory::TimeBasis::Extracted => lvu::TimeBasis::Extracted,
        // A chosen field with no stored token names nothing, so it degrades to
        // capture time rather than restoring a basis that reads no field.
        lvu_memory::TimeBasis::Selected if accepted_field.is_some() => lvu::TimeBasis::Selected,
        lvu_memory::TimeBasis::Selected => lvu::TimeBasis::Capture,
    };
    let legacy_window = match &value.presentation.capture_time {
        Some(lvu_memory::TimePolicy::Recent { seconds }) => {
            lvu::app::TimeWindowChoice::Recent(*seconds)
        }
        Some(lvu_memory::TimePolicy::Absolute { .. }) => lvu::app::TimeWindowChoice::Absolute,
        _ => lvu::app::TimeWindowChoice::All,
    };
    let legacy_start = lvu::app::split_time_draft(&value.presentation.capture_time_start_draft);
    let legacy_end = lvu::app::split_time_draft(&value.presentation.capture_time_end_draft);
    let legacy_draft_present = !value.presentation.capture_time_start_draft.is_empty()
        || !value.presentation.capture_time_end_draft.is_empty();
    let time_draft = value.presentation.time_draft.clone();
    let (
        draft_basis,
        draft_window,
        draft_touched,
        structured_present,
        start_date,
        start_time,
        start_zone,
        end_date,
        end_time,
        end_zone,
        draft_field,
    ) = time_draft.map_or_else(
        || {
            (
                accepted_basis,
                legacy_window,
                legacy_draft_present,
                legacy_draft_present,
                legacy_start.0,
                legacy_start.1,
                legacy_start.2,
                legacy_end.0,
                legacy_end.1,
                legacy_end.2,
                accepted_field.clone(),
            )
        },
        |draft| {
            (
                match draft.basis {
                    lvu_memory::TimeBasis::Selected if draft.field.is_some() => {
                        lvu::TimeBasis::Selected
                    }
                    lvu_memory::TimeBasis::Selected => lvu::TimeBasis::Capture,
                    lvu_memory::TimeBasis::Capture => lvu::TimeBasis::Capture,
                    lvu_memory::TimeBasis::Event => lvu::TimeBasis::Event,
                    lvu_memory::TimeBasis::Extracted => lvu::TimeBasis::Extracted,
                },
                match draft.window {
                    lvu_memory::StoredTimeWindow::All => lvu::app::TimeWindowChoice::All,
                    lvu_memory::StoredTimeWindow::Absolute => lvu::app::TimeWindowChoice::Absolute,
                    lvu_memory::StoredTimeWindow::Recent { seconds } => {
                        lvu::app::TimeWindowChoice::Recent(seconds)
                    }
                    lvu_memory::StoredTimeWindow::DataFirstToLast => {
                        lvu::app::TimeWindowChoice::DataFirstToLast
                    }
                    lvu_memory::StoredTimeWindow::DataRecent { seconds } => {
                        lvu::app::TimeWindowChoice::DataRecent(seconds)
                    }
                    lvu_memory::StoredTimeWindow::AroundSelected { seconds } => {
                        lvu::app::TimeWindowChoice::AroundSelected(match seconds {
                            0 => lvu::app::DEFAULT_AROUND_SECONDS,
                            value => value,
                        })
                    }
                    // A window kind written by a newer build. The rest of the
                    // view is intact; only the draft choice is unknown, so it
                    // reads as no window rather than discarding anything.
                    lvu_memory::StoredTimeWindow::Unknown => lvu::app::TimeWindowChoice::All,
                },
                draft.touched,
                draft.structured_present,
                draft.start_date,
                draft.start_time,
                draft.start_zone,
                draft.end_date,
                draft.end_time,
                draft.end_zone,
                draft.field,
            )
        },
    );
    PersistentViewState {
        time_gap_threshold_seconds: value.presentation.time_gap_threshold_seconds,
        selected_at: value.presentation.selected_at,
        command_steps,
        source_ids: value
            .presentation
            .source_ids
            .iter()
            .map(|id| id.0.to_string())
            .collect(),
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
        // An unknown colour token is a rule written by a newer build: keep the
        // predicate and fall back to the default colour rather than dropping
        // the rule the user wrote.
        color_rules: value
            .presentation
            .color_rules
            .into_iter()
            .take(lvu::MAX_COLOR_RULES)
            .map(|rule| lvu::ColorRule {
                predicate: rule.predicate,
                color: lvu::RuleColor::parse(&rule.color).unwrap_or_default(),
            })
            .collect(),
        fold_enabled: value.presentation.fold_enabled,
        fold_minimum_run: value
            .presentation
            .fold_minimum_run
            .map_or(0, |run| run as usize),
        fold_key_column: value
            .presentation
            .fold_key_column
            .filter(|column| !column.trim().is_empty()),
        fold_lookback: value.presentation.fold_lookback as usize,
        fold_normalisation: lvu::FoldNormalisation::parse_token(
            &value.presentation.fold_normalisation,
        ),
        fold_expanded: value
            .presentation
            .fold_expanded
            .into_iter()
            .map(|id| lvu::RowId::new(id.source_id.0.to_string(), id.sequence))
            .collect(),
        exact_field: value.presentation.exact_field,
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
        applied_time_basis: accepted_basis,
        applied_time_field: (accepted_basis == lvu::TimeBasis::Selected)
            .then_some(accepted_field)
            .flatten(),
        time_field_draft: draft_field,
        time_start_draft: value.presentation.capture_time_start_draft,
        time_end_draft: value.presentation.capture_time_end_draft,
        time_recent_draft: match value.presentation.capture_time {
            Some(lvu_memory::TimePolicy::Recent { seconds }) => {
                lvu::format_capture_duration(seconds)
            }
            _ => String::new(),
        },
        time_error: value.presentation.capture_time_error,
        time_draft_touched: draft_touched,
        time_window_draft: draft_window,
        time_basis_draft: draft_basis,
        time_start_date_draft: start_date,
        time_start_clock_draft: start_time,
        time_start_zone_draft: start_zone,
        time_end_date_draft: end_date,
        time_end_clock_draft: end_time,
        time_end_zone_draft: end_zone,
        time_structured_draft_present: structured_present,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lvu_core::{Acquisition, SourceDefinition};
    use std::time::Instant;
    use tempfile::TempDir;

    #[test]
    fn command_steps_and_their_publications_survive_reopen() {
        let root = TempDir::new().unwrap();
        let view = ViewId::new();
        let mut request = request(1, definition(), view, "accepted search");
        let command = lvu_core::CommandDefinition {
            program: lvu_core::CommandProgram::Exec {
                executable: root.path().join("never-launched"),
                args: vec!["two words".into(), "界".into()],
            },
            cwd: Some(root.path().into()),
            environment: BTreeMap::from([("EXAMPLE".into(), "value".into())]),
            restart: lvu_core::RestartPolicy::Never,
        };
        request.state.applied_enrichments = vec![
            lvu::EnrichmentDefinition::command(
                String::from("command-1"),
                String::from("geo"),
                command.clone(),
            ),
            lvu::EnrichmentDefinition::expression(
                String::from("after"),
                String::from("city = pl.col('geo.city')"),
            ),
        ];
        request.state.command_steps = BTreeMap::from([(
            "command-1".to_owned(),
            lvu::app::CommandStepState {
                revision: 8,
                publication: Some("older independently accepted publication".into()),
            },
        )]);
        let mut store = WorkspaceStore::open(root.path()).unwrap();
        store
            .save_source_and_view(
                &source_metadata(request.definition.clone()),
                &working_view(&request),
                None,
            )
            .unwrap();
        drop(store);
        let store = WorkspaceStore::open(root.path()).unwrap();
        let restored = restored(store.get_view(view).unwrap().unwrap());
        assert_eq!(
            restored.applied_enrichments,
            request.state.applied_enrichments
        );
        assert_eq!(restored.command_steps, request.state.command_steps);
        assert_eq!(restored.applied_search, "accepted search");
    }

    #[test]
    fn applied_colour_rules_survive_sqlite_reopen_in_order() {
        let root = TempDir::new().unwrap();
        let view = ViewId::new();
        let mut request = request(1, definition(), view, "");
        request.state.color_rules = vec![
            lvu::ColorRule {
                predicate: "level: ERROR".into(),
                color: lvu::RuleColor::Red,
            },
            lvu::ColorRule {
                predicate: r"/timeout/i".into(),
                color: lvu::RuleColor::Purple,
            },
        ];
        let mut store = WorkspaceStore::open(root.path()).unwrap();
        store
            .save_source_and_view(
                &source_metadata(request.definition.clone()),
                &working_view(&request),
                None,
            )
            .unwrap();
        drop(store);

        let store = WorkspaceStore::open(root.path()).unwrap();
        let reopened = restored(store.get_view(view).unwrap().unwrap());
        assert_eq!(reopened.color_rules, request.state.color_rules);
    }

    #[test]
    fn a_legacy_command_slot_becomes_the_last_step_of_the_chain() {
        let view = ViewId::new();
        let request = request(1, definition(), view, "search");
        let command = lvu_core::CommandDefinition {
            program: lvu_core::CommandProgram::Exec {
                executable: "/usr/bin/enrich".into(),
                args: vec![],
            },
            cwd: None,
            environment: BTreeMap::new(),
            restart: lvu_core::RestartPolicy::Never,
        };
        let mut working = working_view(&request);
        working.presentation.enrichment_chain = Some(vec![lvu_memory::StoredEnrichment {
            id: "first".into(),
            source: "x = pl.lit(1)".into(),
            command: None,
        }]);
        working.presentation.command_enrichment = Some(lvu_memory::StoredCommandEnrichment {
            id: "command".into(),
            definition: command.clone(),
        });
        working.presentation.command_enrichment_revision = 3;
        working.presentation.command_publication = Some("kept".into());
        let restored = restored(working);
        assert_eq!(restored.applied_enrichments.len(), 2);
        let step = &restored.applied_enrichments[1];
        assert_eq!(step.id.0, "command");
        assert_eq!(step.source, "command");
        assert_eq!(step.command.as_ref(), Some(&command));
        assert_eq!(restored.command_steps["command"].revision, 3);
        assert_eq!(
            restored.command_steps["command"].publication.as_deref(),
            Some("kept")
        );
    }

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
            phase: Arc::new(AtomicU8::new(1)),
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
        let join =
            thread::spawn(move || worker(root, commands_rx, events_tx, Arc::new(AtomicU8::new(0))));

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
    fn flush_drains_a_saturated_event_and_command_queue_before_acknowledging() {
        let temp = TempDir::new().unwrap();
        let (commands_tx, commands_rx) = mpsc::sync_channel(1);
        let (events_tx, events_rx) = mpsc::sync_channel(0);
        let phase = Arc::new(AtomicU8::new(0));
        let worker_phase = Arc::clone(&phase);
        let root = temp.path().to_path_buf();
        let join = thread::spawn(move || worker(root, commands_rx, events_tx, worker_phase));
        let definition = definition();
        let id = ViewId::new();

        // Receiving Recent frees the only command slot. The worker cannot
        // finish it until flush receives its event, so Save then fills that
        // slot deterministically, without relying on scheduling or sleeps.
        commands_tx.send(Command::Recent).unwrap();
        commands_tx
            .send(Command::Save(Box::new(request(1, definition, id, "final"))))
            .unwrap();
        let worker = MemoryWorker {
            tx: commands_tx,
            rx: events_rx,
            _join: join,
            phase,
        };
        let (events, result) = worker.flush(Duration::from_secs(2));
        assert!(result.is_ok(), "{result:?}");
        assert!(matches!(events.first(), Some(Event::Recent(_))));
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::Saved(_, saved, 1) if *saved == id))
        );
        let store = WorkspaceStore::open(temp.path()).unwrap();
        assert_eq!(store.get_view(id).unwrap().unwrap().applied_search, "final");
        worker.stop();
        worker._join.join().unwrap();
    }

    #[test]
    fn expired_flush_distinguishes_queue_admission_from_acknowledgement() {
        let (tx, commands) = mpsc::sync_channel(1);
        let (_events, rx) = mpsc::sync_channel(1);
        tx.try_send(Command::Recent).unwrap();
        let worker = MemoryWorker {
            tx,
            rx,
            _join: thread::spawn(|| {}),
            phase: Arc::new(AtomicU8::new(1)),
        };
        let error = worker.flush(Duration::ZERO).1.unwrap_err();
        assert!(error.contains("waiting to enqueue flush"), "{error}");
        assert!(!error.contains("disconnected"), "{error}");
        assert!(matches!(commands.try_recv(), Ok(Command::Recent)));
        let error = worker.flush(Duration::ZERO).1.unwrap_err();
        assert!(
            error.contains("waiting for flush acknowledgement"),
            "{error}"
        );
        assert!(error.contains("worker: waiting for command"), "{error}");
        assert!(matches!(commands.try_recv(), Ok(Command::Flush(_))));
        worker._join.join().unwrap();
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
                command: None,
            },
            lvu::EnrichmentDefinition {
                id: lvu::EnrichmentStageId("second".into()),
                source: "upper = pl.col('id').str.to_uppercase()".into(),
                command: None,
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
            command: None,
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
    fn automatic_grouping_token_round_trips_without_changing_custom_storage() {
        let temp = TempDir::new().unwrap();
        let worker = MemoryWorker::start(temp.path().to_path_buf());
        let view_id = ViewId::new();
        let mut value = request(1, definition(), view_id, "accepted");
        value.state.applied_grouping = lvu::grouping::AUTO_GROUPING_TOKEN.into();
        value.state.grouping_draft = lvu::grouping::AUTO_GROUPING_TOKEN.into();
        worker.save(Box::new(value)).unwrap();
        assert!(worker.flush(Duration::from_secs(1)).1.is_ok());

        let stored = WorkspaceStore::open(temp.path())
            .unwrap()
            .get_view(view_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            stored.presentation.applied_grouping.as_deref(),
            Some(lvu::grouping::AUTO_GROUPING_TOKEN)
        );
        let restored = super::restored(stored);
        assert_eq!(
            restored.applied_grouping,
            lvu::grouping::AUTO_GROUPING_TOKEN
        );
        assert_eq!(restored.grouping_draft, lvu::grouping::AUTO_GROUPING_TOKEN);
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
    fn segmented_time_draft_survives_sqlite_reopen_without_changing_accepted_policy() {
        let root = TempDir::new().unwrap();
        let view_id = ViewId::new();
        let mut value = request(1, definition(), view_id, "accepted");
        value.state.applied_capture_time_policy =
            Some(lvu::CaptureTimePolicy::Recent { seconds: 900 });
        value.state.applied_time_basis = lvu::TimeBasis::Capture;
        value.state.time_basis_draft = lvu::TimeBasis::Extracted;
        value.state.time_window_draft = lvu::app::TimeWindowChoice::Recent(37);
        value.state.time_draft_touched = true;
        value.state.time_structured_draft_present = true;
        value.state.time_start_date_draft = String::new();
        value.state.time_start_clock_draft = "12:34:56.123456789".into();
        value.state.time_start_zone_draft = "+05:45".into();
        value.state.time_end_date_draft = "2026-09-06".into();
        value.state.time_end_clock_draft = String::new();
        value.state.time_end_zone_draft = "-03:30".into();
        value.state.time_error = Some("unfinished segmented edit".into());

        let mut store = WorkspaceStore::open(root.path()).unwrap();
        store
            .save_source_and_view(
                &source_metadata(value.definition.clone()),
                &working_view(&value),
                None,
            )
            .unwrap();
        drop(store);

        let store = WorkspaceStore::open(root.path()).unwrap();
        let restored = restored(store.get_view(view_id).unwrap().unwrap());
        assert_eq!(
            restored.applied_capture_time_policy,
            Some(lvu::CaptureTimePolicy::Recent { seconds: 900 })
        );
        assert_eq!(restored.applied_time_basis, lvu::TimeBasis::Capture);
        assert_eq!(restored.time_basis_draft, lvu::TimeBasis::Extracted);
        assert_eq!(
            restored.time_window_draft,
            lvu::app::TimeWindowChoice::Recent(37)
        );
        assert!(restored.time_draft_touched);
        assert!(restored.time_structured_draft_present);
        assert_eq!(restored.time_start_date_draft, "");
        assert_eq!(restored.time_start_clock_draft, "12:34:56.123456789");
        assert_eq!(restored.time_start_zone_draft, "+05:45");
        assert_eq!(restored.time_end_date_draft, "2026-09-06");
        assert_eq!(restored.time_end_clock_draft, "");
        assert_eq!(restored.time_end_zone_draft, "-03:30");
        assert_eq!(
            restored.time_error.as_deref(),
            Some("unfinished segmented edit")
        );
    }

    #[test]
    fn legacy_presentation_json_migrates_combined_time_drafts_after_sqlite_reopen() {
        let root = TempDir::new().unwrap();
        let view_id = ViewId::new();
        let value = request(1, definition(), view_id, "accepted");
        let mut stored = working_view(&value);
        stored.presentation.time_basis = lvu_memory::TimeBasis::Event;
        stored.presentation.capture_time = Some(lvu_memory::TimePolicy::Recent { seconds: 73 });
        stored.presentation.capture_time_start_draft = "2026-09-06T12:34:56.123456789+05:45".into();
        stored.presentation.capture_time_end_draft = "2026-09-06T08:19:56.000000001Z".into();
        let mut legacy_json = serde_json::to_value(&stored.presentation).unwrap();
        legacy_json.as_object_mut().unwrap().remove("time_draft");
        stored.presentation = serde_json::from_value(legacy_json).unwrap();
        assert!(stored.presentation.time_draft.is_none());

        let mut store = WorkspaceStore::open(root.path()).unwrap();
        store
            .save_source_and_view(&source_metadata(value.definition), &stored, None)
            .unwrap();
        drop(store);

        let store = WorkspaceStore::open(root.path()).unwrap();
        let restored = restored(store.get_view(view_id).unwrap().unwrap());
        assert_eq!(restored.time_basis_draft, lvu::TimeBasis::Event);
        assert_eq!(
            restored.time_window_draft,
            lvu::app::TimeWindowChoice::Recent(73)
        );
        assert!(restored.time_draft_touched);
        assert!(restored.time_structured_draft_present);
        assert_eq!(restored.time_start_date_draft, "2026-09-06");
        assert_eq!(restored.time_start_clock_draft, "12:34:56.123456789");
        assert_eq!(restored.time_start_zone_draft, "+05:45");
        assert_eq!(restored.time_end_date_draft, "2026-09-06");
        assert_eq!(restored.time_end_clock_draft, "08:19:56.000000001");
        assert_eq!(restored.time_end_zone_draft, "Z");
        assert_eq!(
            restored.applied_capture_time_policy,
            Some(lvu::CaptureTimePolicy::Recent { seconds: 73 })
        );
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
                exact_field: Some(
                    lvu_core::FieldCorrelation::new(
                        "request_id",
                        lvu_core::ExactScalar::UnsignedInteger(u64::MAX),
                        [(definition.id.0.to_string(), "req".to_owned())]
                            .into_iter()
                            .collect(),
                    )
                    .unwrap(),
                ),
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
        let reopened = restored(store.get_view(id).unwrap().unwrap());
        assert_eq!(reopened.bookmarks, request.state.bookmarks);
        assert_eq!(reopened.exact_field, request.state.exact_field);
        request.state.bookmarks[0].id.source_id = SourceId::new().0.to_string();
        assert!(store.update_view(&working_view(&request), 0).is_err());
        request.state.bookmarks[0].id.source_id = definition.id.0.to_string();
        request.state.bookmarks[0].note = "x".repeat(1025);
        assert!(store.update_view(&working_view(&request), 0).is_err());
    }
}
