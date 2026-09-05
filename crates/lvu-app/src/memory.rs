use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::mpsc::{self, Receiver, SyncSender, TrySendError},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use lvu::PersistentViewState;
use lvu_core::{RecordId, SourceDefinition, SourceId, ViewId};
use lvu_memory::{
    DraftState, NavigationState, PresentationState, SourceMetadata, WorkingView, WorkspaceStore,
};

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
    Flush(SyncSender<Result<(), String>>),
    Stop,
}
pub enum Event {
    Loaded(SourceId, ViewId, Box<Option<WorkingView>>),
    LoadFailed(SourceId, ViewId, String),
    Saved(SourceId, ViewId, u64),
    SaveFailed(SourceId, ViewId, u64, String),
    Recent(Vec<SourceMetadata>),
    RecentFailed(String),
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
    while let Ok(command) = commands.recv() {
        match command {
            Command::Load(definition, view_id) => {
                let definition = *definition;
                let result = (|| {
                    let metadata = source_metadata(definition.clone());
                    store.upsert_source(&metadata)?;
                    let view = store.working_view_for_source(definition.id)?;
                    if let Some(value) = &view {
                        versions.insert(value.id, value.version);
                    }
                    Ok::<_, lvu_memory::MemoryError>(view)
                })();
                match result {
                    Ok(view) => {
                        if events
                            .send(Event::Loaded(definition.id, view_id, Box::new(view)))
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
                match store.save_source_and_view(&metadata, &view, expected) {
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
            Command::Flush(done) => {
                let result = if failed.is_empty() {
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
        name: "Raw events".into(),
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
            pinned_columns: request.state.pinned_columns.clone(),
            color_field: request.state.color_field.clone(),
        },
        version: 0,
    }
}
fn nonempty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_owned())
}

pub fn restored(value: WorkingView) -> PersistentViewState {
    PersistentViewState {
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
        let worker = MemoryWorker::start_with_capacities(temp.path().to_path_buf(), 4, 1);
        worker.recent().unwrap();
        thread::sleep(Duration::from_millis(20));
        let definition = definition();
        let source_id = definition.id;
        let view_id = ViewId::new();
        worker.load(definition, view_id).unwrap();
        thread::sleep(Duration::from_millis(20));

        assert!(matches!(worker.poll(), Some(Event::Recent(_))));
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if let Some(Event::Loaded(id, requested, _)) = worker.poll() {
                assert_eq!(id, source_id);
                assert_eq!(requested, view_id);
                break;
            }
            assert!(Instant::now() < deadline, "load completion was lost");
            thread::sleep(Duration::from_millis(2));
        }
        worker.stop();
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
        assert_eq!(stored.advanced_filter_draft.unwrap().text, "pl.col(");
        assert_eq!(stored.presentation.pinned_columns, ["service"]);
        assert_eq!(
            stored.presentation.color_field.as_deref(),
            Some("request_id")
        );
        worker.stop();
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
