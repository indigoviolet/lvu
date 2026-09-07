//! Explicit command-enrichment orchestration. Blocking work never runs on the UI tick.

use super::{Composition, MemoryEvent, SaveRequest};
use crate::{command_execution, command_rows, command_snapshot};
use lvu::App;
use lvu::app::{
    CommandEnrichmentRequest, CommandEnrichmentReview, CommandEnrichmentRunState,
    CommandEnrichmentStage,
};
use lvu_core::{CommandProgram, ViewId};
use lvu_memory::{CommandAttemptScope, WorkspaceStore};
use lvu_view::NativeViewAdapter;
use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
use uuid::Uuid;

const SCOPE_NAMESPACE: Uuid = Uuid::from_bytes([
    0xf1, 0x4c, 0xb4, 0x21, 0x19, 0x0d, 0x48, 0xb8, 0x97, 0x34, 0xe1, 0x25, 0x80, 0x27, 0x95, 0x44,
]);

pub(super) struct CommandController {
    workspace: PathBuf,
    cwd: PathBuf,
    presentation: command_rows::CommandPresentation,
    active: Option<Active>,
    persistence: Option<Persistence>,
    observed: HashMap<String, Option<String>>,
    restore_queue: VecDeque<RestoreRequest>,
}

enum Active {
    Preparing {
        generation: u64,
        view: String,
        revision: u64,
        native_fingerprint: String,
        cancel: Arc<AtomicBool>,
        rx: mpsc::Receiver<Result<command_snapshot::PreparedCommand, String>>,
        worker: JoinHandle<()>,
    },
    Ready {
        generation: u64,
        view: String,
        revision: u64,
        native_fingerprint: String,
        token: String,
        prepared: command_snapshot::PreparedCommand,
    },
    Executing {
        generation: u64,
        view: String,
        revision: u64,
        native_fingerprint: String,
        scope: CommandAttemptScope,
        ids: Vec<lvu_core::RecordId>,
        cancel: Arc<AtomicBool>,
        rx: mpsc::Receiver<Result<command_execution::CommandExecutionResult, String>>,
        worker: JoinHandle<()>,
    },
    Restoring {
        request: RestoreRequest,
        cancel: Arc<AtomicBool>,
        rx: mpsc::Receiver<Result<command_rows::CommandResults, String>>,
        worker: JoinHandle<()>,
    },
}

#[derive(Clone)]
struct RestoreRequest {
    view: String,
    reference: String,
}

enum PersistKind {
    Definition {
        generation: u64,
        stage: Option<CommandEnrichmentStage>,
    },
    Publication {
        generation: u64,
        native_fingerprint: String,
        reference: String,
        rows: command_rows::CommandResults,
        record_count: usize,
    },
}

struct Persistence {
    view: String,
    revision: u64,
    sequence: Option<u64>,
    request: Option<Box<SaveRequest>>,
    kind: PersistKind,
}

impl CommandController {
    pub(super) fn new(
        workspace: PathBuf,
        cwd: PathBuf,
        presentation: command_rows::CommandPresentation,
    ) -> Self {
        Self {
            workspace,
            cwd,
            presentation,
            active: None,
            persistence: None,
            observed: HashMap::new(),
            restore_queue: VecDeque::new(),
        }
    }

    pub(super) fn suppresses_autosave(&self, view: ViewId) -> bool {
        self.persistence
            .as_ref()
            .is_some_and(|pending| Uuid::parse_str(&pending.view).ok().map(ViewId) == Some(view))
    }

    fn busy(&self) -> bool {
        self.active.is_some() || self.persistence.is_some()
    }

    fn cancel_active(&mut self, generation: u64, view: &str) {
        let matches = match self.active.as_ref() {
            Some(Active::Preparing {
                generation: g,
                view: v,
                ..
            })
            | Some(Active::Ready {
                generation: g,
                view: v,
                ..
            })
            | Some(Active::Executing {
                generation: g,
                view: v,
                ..
            }) => *g == generation && v == view,
            _ => false,
        };
        if matches {
            if let Some(Active::Preparing { cancel, .. } | Active::Executing { cancel, .. }) =
                &self.active
            {
                cancel.store(true, Ordering::Release);
            }
            if matches!(self.active, Some(Active::Ready { .. })) {
                self.active = None;
            }
        }
    }

    pub(super) fn shutdown(&mut self, timeout: Duration) -> Result<(), String> {
        if let Some(
            Active::Preparing { cancel, .. }
            | Active::Executing { cancel, .. }
            | Active::Restoring { cancel, .. },
        ) = &self.active
        {
            cancel.store(true, Ordering::Release);
        }
        let deadline = Instant::now() + timeout;
        while self.active.as_ref().is_some_and(active_running) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
        if self.active.as_ref().is_some_and(active_running) {
            return Err("command worker did not stop before shutdown deadline".into());
        }
        join_active(self.active.take());
        Ok(())
    }
}

fn active_running(active: &Active) -> bool {
    match active {
        Active::Preparing { worker, .. }
        | Active::Executing { worker, .. }
        | Active::Restoring { worker, .. } => !worker.is_finished(),
        Active::Ready { .. } => false,
    }
}

fn join_active(active: Option<Active>) {
    if let Some(
        Active::Preparing { worker, .. }
        | Active::Executing { worker, .. }
        | Active::Restoring { worker, .. },
    ) = active
    {
        let _ = worker.join();
    }
}

impl Composition {
    pub(super) fn flush_command_persistence(
        &mut self,
        app: &mut App,
        adapter: &NativeViewAdapter,
        timeout: Duration,
    ) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        while self.command_controller.persistence.is_some() {
            self.dispatch_command_persistence(app);
            self.poll_memory(app, adapter);
            if Instant::now() >= deadline {
                if let Some(pending) = self.command_controller.persistence.take() {
                    fail_persistence(
                        app,
                        pending,
                        "durable command save did not settle before shutdown".into(),
                    );
                }
                return Err("durable command save did not settle before shutdown".into());
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        Ok(())
    }

    pub(super) fn handle_command_enrichment(
        &mut self,
        app: &mut App,
        adapter: &NativeViewAdapter,
    ) -> bool {
        let requests = app.take_command_enrichment_requests();
        let mut changed = !requests.is_empty();
        for request in requests {
            match request {
                CommandEnrichmentRequest::Cancel {
                    generation,
                    view_id,
                } => {
                    let cancels_queued_publication = self
                        .command_controller
                        .persistence
                        .as_ref()
                        .is_some_and(|pending| {
                            queued_publication_matches(pending, generation, &view_id)
                        });
                    if cancels_queued_publication {
                        let pending = self
                            .command_controller
                            .persistence
                            .take()
                            .expect("queued publication");
                        fail_persistence(
                            app,
                            pending,
                            "command result save was cancelled before commit".into(),
                        );
                    } else {
                        self.command_controller.cancel_active(generation, &view_id);
                    }
                }
                CommandEnrichmentRequest::Save {
                    generation,
                    view_id,
                    base_definition_revision,
                    candidate,
                } => {
                    if self.command_controller.busy() {
                        app.finish_command_enrichment_save(
                            generation,
                            &view_id,
                            base_definition_revision.saturating_add(1),
                            Err("another command operation is active".into()),
                        );
                        continue;
                    }
                    let Some(state) = app.persistent_view_state(&view_id) else {
                        app.finish_command_enrichment_save(
                            generation,
                            &view_id,
                            base_definition_revision.saturating_add(1),
                            Err("view is unavailable".into()),
                        );
                        continue;
                    };
                    if state.command_enrichment_revision != base_definition_revision {
                        app.finish_command_enrichment_save(
                            generation,
                            &view_id,
                            base_definition_revision.saturating_add(1),
                            Err("command definition changed; reopen the editor".into()),
                        );
                        continue;
                    }
                    self.command_controller.persistence = Some(Persistence {
                        view: view_id,
                        revision: base_definition_revision.saturating_add(1),
                        sequence: None,
                        request: None,
                        kind: PersistKind::Definition {
                            generation,
                            stage: candidate,
                        },
                    });
                }
                CommandEnrichmentRequest::PrepareRun {
                    generation,
                    view_id,
                    definition_revision,
                    stage_id,
                } => {
                    if self.command_controller.busy() {
                        app.finish_command_enrichment_review(
                            generation,
                            &view_id,
                            definition_revision,
                            Err("another command operation is active".into()),
                        );
                        continue;
                    }
                    if app.view_has_pending_query(&view_id) {
                        app.finish_command_enrichment_review(generation, &view_id, definition_revision, Err("native view changes are still pending; wait before reviewing a run".into()));
                        continue;
                    }
                    let Some(state) = app.persistent_view_state(&view_id) else {
                        continue;
                    };
                    let Some(stage) = state.command_enrichment.filter(|stage| {
                        stage.id == stage_id
                            && state.command_enrichment_revision == definition_revision
                    }) else {
                        app.finish_command_enrichment_review(
                            generation,
                            &view_id,
                            definition_revision,
                            Err("saved command definition is stale".into()),
                        );
                        continue;
                    };
                    let native_fingerprint = native_fingerprint(&state.applied_enrichments);
                    let frozen =
                        match adapter.freeze_input(&view_id, command_snapshot::input_limits()) {
                            Ok(value) => value,
                            Err(error) => {
                                app.finish_command_enrichment_review(
                                    generation,
                                    &view_id,
                                    definition_revision,
                                    Err(error.to_string()),
                                );
                                continue;
                            }
                        };
                    let cancel = Arc::new(AtomicBool::new(false));
                    let worker_cancel = Arc::clone(&cancel);
                    let cwd = self.command_controller.cwd.clone();
                    let native = state.applied_enrichments;
                    let worker_view = view_id.clone();
                    let (tx, rx) = mpsc::sync_channel(1);
                    let worker = std::thread::spawn(move || {
                        let result = (|| {
                            let mut effective = stage;
                            normalize_stage(&cwd, &mut effective)?;
                            let scope = command_scope(&worker_view, &effective, &native)?;
                            command_snapshot::prepare(
                                frozen,
                                scope,
                                effective.definition,
                                &worker_cancel,
                            )
                        })();
                        let _ = tx.send(result);
                    });
                    self.command_controller.active = Some(Active::Preparing {
                        generation,
                        view: view_id,
                        revision: definition_revision,
                        native_fingerprint,
                        cancel,
                        rx,
                        worker,
                    });
                }
                CommandEnrichmentRequest::Execute {
                    generation,
                    view_id,
                    definition_revision,
                    review_token,
                } => {
                    let ready = self.command_controller.active.take();
                    let Some(Active::Ready {
                        generation: g,
                        view,
                        revision,
                        native_fingerprint,
                        token,
                        prepared,
                    }) = ready
                    else {
                        self.command_controller.active = ready;
                        app.finish_command_enrichment_run(
                            generation,
                            &view_id,
                            definition_revision,
                            Err("reviewed command input is no longer available".into()),
                        );
                        continue;
                    };
                    if (g, view.as_str(), revision, token.as_str())
                        != (
                            generation,
                            view_id.as_str(),
                            definition_revision,
                            review_token.as_str(),
                        )
                        || !command_fence(app, &view, revision, &native_fingerprint)
                    {
                        self.command_controller.active = Some(Active::Ready {
                            generation: g,
                            view,
                            revision,
                            native_fingerprint,
                            token,
                            prepared,
                        });
                        app.finish_command_enrichment_run(
                            generation,
                            &view_id,
                            definition_revision,
                            Err("review token is stale".into()),
                        );
                        continue;
                    }
                    let scope = prepared.scope.clone();
                    let ids = prepared
                        .events
                        .iter()
                        .map(|event| event.record.record_id)
                        .collect();
                    let workspace = self.command_controller.workspace.clone();
                    let cancel = Arc::new(AtomicBool::new(false));
                    let worker_cancel = Arc::clone(&cancel);
                    let (tx, rx) = mpsc::sync_channel(1);
                    let worker = std::thread::spawn(move || {
                        let result = WorkspaceStore::open(workspace)
                            .map_err(|e| e.to_string())
                            .and_then(|mut store| {
                                command_execution::execute_command_enrichment(
                                    &mut store,
                                    prepared.scope,
                                    prepared.definition,
                                    prepared.events,
                                    &worker_cancel,
                                )
                                .map_err(|e| e.to_string())
                            });
                        let _ = tx.send(result);
                    });
                    self.command_controller.active = Some(Active::Executing {
                        generation,
                        view,
                        revision,
                        native_fingerprint,
                        scope,
                        ids,
                        cancel,
                        rx,
                        worker,
                    });
                }
            }
        }
        changed |= self.poll_command_worker(app);
        changed |= self.reconcile_ready(app);
        changed |= self.sync_command_restores(app);
        changed |= self.dispatch_command_persistence(app);
        changed
    }

    fn poll_command_worker(&mut self, app: &mut App) -> bool {
        let Some(active) = self.command_controller.active.take() else {
            return false;
        };
        match active {
            Active::Preparing {
                generation,
                view,
                revision,
                native_fingerprint,
                cancel,
                rx,
                worker,
            } => match rx.try_recv() {
                Ok(result) => {
                    let _ = worker.join();
                    match result {
                        Ok(prepared) => {
                            let token = Uuid::new_v4().to_string();
                            let review = review(&prepared, token.clone());
                            if !cancel.load(Ordering::Acquire)
                                && command_fence(app, &view, revision, &native_fingerprint)
                                && app.finish_command_enrichment_review(
                                    generation,
                                    &view,
                                    revision,
                                    Ok(review),
                                )
                            {
                                self.command_controller.active = Some(Active::Ready {
                                    generation,
                                    view,
                                    revision,
                                    native_fingerprint,
                                    token,
                                    prepared,
                                });
                            }
                        }
                        Err(error) => {
                            app.finish_command_enrichment_review(
                                generation,
                                &view,
                                revision,
                                Err(error),
                            );
                        }
                    }
                    true
                }
                Err(mpsc::TryRecvError::Empty) => {
                    self.command_controller.active = Some(Active::Preparing {
                        generation,
                        view,
                        revision,
                        native_fingerprint,
                        cancel,
                        rx,
                        worker,
                    });
                    false
                }
                Err(_) => {
                    let _ = worker.join();
                    app.finish_command_enrichment_review(
                        generation,
                        &view,
                        revision,
                        Err("command preparation worker stopped".into()),
                    );
                    true
                }
            },
            Active::Executing {
                generation,
                view,
                revision,
                native_fingerprint,
                scope,
                ids,
                cancel,
                rx,
                worker,
            } => match rx.try_recv() {
                Ok(result) => {
                    let _ = worker.join();
                    match result {
                        Ok(result)
                            if !cancel.load(Ordering::Acquire)
                                && command_fence(app, &view, revision, &native_fingerprint)
                                && dialog_running(app, generation, &view, revision) =>
                        {
                            let rows = result
                                .records
                                .into_iter()
                                .map(|record| {
                                    (
                                        record.record_id,
                                        command_rows::CommandResult {
                                            fields: record.fields,
                                            diagnostic: record.diagnostic,
                                        },
                                    )
                                })
                                .collect();
                            let admitted = self
                                .command_controller
                                .presentation
                                .can_publish(&view, &rows)
                                .and_then(|()| {
                                    command_snapshot::PublicationReference::new(scope, ids).encode()
                                });
                            match admitted {
                                Ok(reference) => {
                                    self.command_controller.persistence = Some(Persistence {
                                        view,
                                        revision,
                                        sequence: None,
                                        request: None,
                                        kind: PersistKind::Publication {
                                            generation,
                                            native_fingerprint,
                                            reference,
                                            record_count: rows.len(),
                                            rows,
                                        },
                                    })
                                }
                                Err(error) => {
                                    app.finish_command_enrichment_run(
                                        generation,
                                        &view,
                                        revision,
                                        Err(format!(
                                            "Previous published results retained: {error}"
                                        )),
                                    );
                                }
                            }
                        }
                        Ok(_) => {
                            app.finish_command_enrichment_run(
                                generation,
                                &view,
                                revision,
                                Err("Previous published results retained: command result became stale or was cancelled".into()),
                            );
                        }
                        Err(error) => {
                            app.finish_command_enrichment_run(
                                generation,
                                &view,
                                revision,
                                Err(format!("Previous published results retained: {error}")),
                            );
                        }
                    }
                    true
                }
                Err(mpsc::TryRecvError::Empty) => {
                    self.command_controller.active = Some(Active::Executing {
                        generation,
                        view,
                        revision,
                        native_fingerprint,
                        scope,
                        ids,
                        cancel,
                        rx,
                        worker,
                    });
                    false
                }
                Err(_) => {
                    let _ = worker.join();
                    app.finish_command_enrichment_run(
                        generation,
                        &view,
                        revision,
                        Err("Previous published results retained: command worker stopped".into()),
                    );
                    true
                }
            },
            Active::Restoring {
                request,
                cancel,
                rx,
                worker,
            } => match rx.try_recv() {
                Ok(result) => {
                    let _ = worker.join();
                    if app
                        .persistent_view_state(&request.view)
                        .is_some_and(|state| {
                            state.command_publication.as_deref() == Some(&request.reference)
                        })
                        && let Ok(rows) = result
                    {
                        if let Err(error) = self
                            .command_controller
                            .presentation
                            .publish(&request.view, rows)
                        {
                            app.action_notice = Some(bounded(
                                format!("Saved command results unavailable: {error}"),
                                1024,
                            ));
                        }
                    } else if let Err(error) = result {
                        app.action_notice = Some(bounded(
                            format!("Saved command results unavailable: {error}"),
                            1024,
                        ));
                    }
                    true
                }
                Err(mpsc::TryRecvError::Empty) => {
                    self.command_controller.active = Some(Active::Restoring {
                        request,
                        cancel,
                        rx,
                        worker,
                    });
                    false
                }
                Err(_) => {
                    let _ = worker.join();
                    app.action_notice =
                        Some("Saved command results unavailable: restore worker stopped".into());
                    true
                }
            },
            ready @ Active::Ready { .. } => {
                self.command_controller.active = Some(ready);
                false
            }
        }
    }

    fn reconcile_ready(&mut self, app: &App) -> bool {
        let stale = match self.command_controller.active.as_ref() {
            Some(Active::Ready {
                generation,
                view,
                revision,
                native_fingerprint,
                token,
                ..
            }) => {
                !command_fence(app, view, *revision, native_fingerprint)
                    || !app.layers.external_command.state().is_some_and(|dialog| {
                        dialog.generation == *generation
                            && dialog.view_id == *view
                            && dialog.base_definition_revision == *revision
                            && dialog.run_state == CommandEnrichmentRunState::Ready
                            && dialog
                                .review
                                .as_ref()
                                .is_some_and(|review| review.review_token == *token)
                    })
            }
            _ => false,
        };
        if stale {
            self.command_controller.active = None;
        }
        stale
    }

    fn sync_command_restores(&mut self, app: &mut App) -> bool {
        let open = app
            .views()
            .iter()
            .map(|view| view.id.as_str())
            .collect::<std::collections::HashSet<_>>();
        self.command_controller
            .observed
            .retain(|view, _| open.contains(view.as_str()));
        self.command_controller.restore_queue.retain(|request| {
            open.contains(request.view.as_str())
                && app
                    .persistent_view_state(&request.view)
                    .is_some_and(|state| {
                        state.command_publication.as_deref() == Some(request.reference.as_str())
                    })
        });
        self.command_controller
            .presentation
            .configure(app.views().iter().filter_map(|view| {
                app.persistent_view_state(&view.id)
                    .map(|state| (view.id.clone(), state.command_enrichment.is_some()))
            }));
        let mut restore_queue_full = false;
        for view in app.views() {
            let Some(state) = app.persistent_view_state(&view.id) else {
                continue;
            };
            let reference = state.command_publication.clone();
            // Last-good publication is independent of later definition revisions.
            // Reload only when its actual immutable reference changes.
            if self.command_controller.observed.get(&view.id) == Some(&reference) {
                continue;
            }
            if let Some(reference) = reference {
                let duplicate = self
                    .command_controller
                    .restore_queue
                    .iter()
                    .any(|queued| queued.view == view.id && queued.reference == reference);
                if duplicate {
                    self.command_controller
                        .observed
                        .insert(view.id.clone(), Some(reference));
                } else if self.command_controller.restore_queue.len() < 128 {
                    self.command_controller
                        .restore_queue
                        .push_back(RestoreRequest {
                            view: view.id.clone(),
                            reference,
                        });
                    self.command_controller
                        .observed
                        .insert(view.id.clone(), state.command_publication.clone());
                } else {
                    restore_queue_full = true;
                }
            } else {
                self.command_controller
                    .observed
                    .insert(view.id.clone(), None);
                let _ = self
                    .command_controller
                    .presentation
                    .publish(&view.id, Default::default());
            }
        }
        if restore_queue_full {
            app.action_notice =
                Some("command result restore queue is full; results remain pending".into());
        }
        if !self.command_controller.busy()
            && let Some(request) = self.command_controller.restore_queue.pop_front()
        {
            let decoded =
                command_snapshot::PublicationReference::decode(&request.reference, &request.view);
            match decoded {
                Ok(reference) => {
                    let workspace = self.command_controller.workspace.clone();
                    let cancel = Arc::new(AtomicBool::new(false));
                    let worker_cancel = Arc::clone(&cancel);
                    let (tx, rx) = mpsc::sync_channel(1);
                    let worker = std::thread::spawn(move || {
                        let result = WorkspaceStore::open(workspace)
                            .map_err(|e| e.to_string())
                            .and_then(|store| reference.restore(&store, &worker_cancel));
                        let _ = tx.send(result);
                    });
                    self.command_controller.active = Some(Active::Restoring {
                        request,
                        cancel,
                        rx,
                        worker,
                    });
                }
                Err(error) => {
                    app.action_notice = Some(bounded(
                        format!("Saved command results unavailable: {error}"),
                        1024,
                    ));
                }
            }
        }
        false
    }

    fn dispatch_command_persistence(&mut self, app: &mut App) -> bool {
        let Some(mut pending) = self.command_controller.persistence.take() else {
            return false;
        };
        if pending.sequence.is_some() {
            self.command_controller.persistence = Some(pending);
            return false;
        }
        let view_id = match Uuid::parse_str(&pending.view).map(ViewId) {
            Ok(value) => value,
            Err(error) => {
                fail_persistence(app, pending, format!("invalid view identity: {error}"));
                return true;
            }
        };
        if self.memory_inflight.values().any(|(id, _)| *id == view_id) {
            self.command_controller.persistence = Some(pending);
            return false;
        }
        if let PersistKind::Publication {
            generation,
            native_fingerprint,
            ..
        } = &pending.kind
            && (!dialog_running(app, *generation, &pending.view, pending.revision)
                || !command_fence(app, &pending.view, pending.revision, native_fingerprint))
        {
            fail_persistence(
                app,
                pending,
                "command result became stale or was cancelled before commit".into(),
            );
            return true;
        }
        let Some(view) = app.views().iter().find(|view| view.id == pending.view) else {
            fail_persistence(app, pending, "view closed before durable save".into());
            return true;
        };
        let definition = match Uuid::parse_str(&view.source_id)
            .map(lvu_core::SourceId)
            .ok()
            .and_then(|source| self.definitions.get(&source).cloned())
        {
            Some(value) => value,
            None => {
                fail_persistence(
                    app,
                    pending,
                    "source definition unavailable before durable save".into(),
                );
                return true;
            }
        };
        let Some(mut state) = app.persistent_view_state(&pending.view) else {
            fail_persistence(
                app,
                pending,
                "view state unavailable before durable save".into(),
            );
            return true;
        };
        if let Err(error) = apply_persistence_patch(&mut state, &pending) {
            fail_persistence(app, pending, error);
            return true;
        }
        let sequence = pending.request.as_ref().map_or_else(
            || {
                self.memory_sequence = self.memory_sequence.saturating_add(1);
                self.memory_sequence
            },
            |request| request.sequence,
        );
        // A rejected queue admission may be retried after UI/native state changes.
        // Always rebuild the full save from the freshest accepted view state.
        pending.request = Some(Box::new(SaveRequest {
            sequence,
            definition,
            view_id,
            state,
        }));
        self.memory_pending.remove(&view_id);
        let request = pending.request.take().expect("controller save request");
        let sequence = request.sequence;
        let state = request.state.clone();
        match self.memory.save(request) {
            Ok(()) => {
                self.memory_inflight.insert(sequence, (view_id, state));
                pending.sequence = Some(sequence);
                if let PersistKind::Publication { generation, .. } = &pending.kind {
                    let entered =
                        app.begin_command_result_save(*generation, &pending.view, pending.revision);
                    debug_assert!(
                        entered,
                        "publication commit point must match running dialog"
                    );
                }
            }
            Err(request) => pending.request = Some(request),
        }
        self.command_controller.persistence = Some(pending);
        true
    }

    pub(super) fn handle_command_memory_event(&mut self, app: &mut App, event: &MemoryEvent) {
        let Some(pending) = self.command_controller.persistence.as_ref() else {
            return;
        };
        let sequence = match event {
            MemoryEvent::Saved(_, _, sequence) | MemoryEvent::SaveFailed(_, _, sequence, _) => {
                *sequence
            }
            MemoryEvent::Fatal(error) => {
                let pending = self
                    .command_controller
                    .persistence
                    .take()
                    .expect("pending command persistence");
                fail_persistence(app, pending, error.clone());
                return;
            }
            _ => return,
        };
        if pending.sequence != Some(sequence) {
            return;
        }
        let pending = self
            .command_controller
            .persistence
            .take()
            .expect("command persistence");
        match (event, pending.kind) {
            (MemoryEvent::Saved(..), PersistKind::Definition { generation, stage }) => {
                app.finish_command_enrichment_save(
                    generation,
                    &pending.view,
                    pending.revision,
                    Ok(stage),
                );
            }
            (
                MemoryEvent::Saved(..),
                PersistKind::Publication {
                    generation,
                    native_fingerprint: _,
                    reference,
                    rows,
                    record_count,
                },
            ) => {
                if app.commit_command_publication(
                    &pending.view,
                    pending.revision,
                    reference.clone(),
                ) {
                    self.command_controller
                        .observed
                        .insert(pending.view.clone(), Some(reference));
                    // Controller publication is globally serialized, so nothing can
                    // invalidate can_publish admission before this matching publish.
                    match self
                        .command_controller
                        .presentation
                        .publish(&pending.view, rows)
                    {
                        Ok(()) => {
                            let status =
                                format!("Published {record_count} durable command results");
                            if !app.finish_command_enrichment_run(
                                generation,
                                &pending.view,
                                pending.revision,
                                Ok(status.clone()),
                            ) {
                                app.action_notice = Some(status);
                            }
                        }
                        Err(error) => {
                            let message = format!("Previous published results retained: {error}");
                            if !app.finish_command_enrichment_run(
                                generation,
                                &pending.view,
                                pending.revision,
                                Err(message.clone()),
                            ) {
                                app.action_notice = Some(bounded(message, 1024));
                            }
                        }
                    }
                } else {
                    app.action_notice = Some(
                        "Durable command results were saved but could not be attached to the view"
                            .into(),
                    );
                }
            }
            (
                MemoryEvent::SaveFailed(_, _, _, error),
                PersistKind::Definition { generation, .. },
            ) => {
                app.finish_command_enrichment_save(
                    generation,
                    &pending.view,
                    pending.revision,
                    Err(error.clone()),
                );
            }
            (
                MemoryEvent::SaveFailed(_, _, _, error),
                PersistKind::Publication { generation, .. },
            ) => {
                let message = format!("Previous published results retained: {error}");
                if !app.finish_command_enrichment_run(
                    generation,
                    &pending.view,
                    pending.revision,
                    Err(message.clone()),
                ) {
                    app.action_notice = Some(bounded(message, 1024));
                }
            }
            _ => {}
        }
    }
}

fn apply_persistence_patch(
    state: &mut lvu::PersistentViewState,
    pending: &Persistence,
) -> Result<(), String> {
    match &pending.kind {
        PersistKind::Definition { stage, .. } => {
            if state.command_enrichment_revision.saturating_add(1) != pending.revision {
                return Err("command definition changed before durable save".into());
            }
            state.command_enrichment = stage.clone();
            state.command_enrichment_revision = pending.revision;
        }
        PersistKind::Publication {
            native_fingerprint: expected_fingerprint,
            reference,
            ..
        } => {
            if state.command_enrichment_revision != pending.revision
                || native_fingerprint(&state.applied_enrichments) != *expected_fingerprint
            {
                return Err("command definition changed before publication".into());
            }
            state.command_publication = Some(reference.clone());
        }
    }
    Ok(())
}

fn queued_publication_matches(pending: &Persistence, generation: u64, view: &str) -> bool {
    pending.sequence.is_none()
        && pending.view == view
        && matches!(
            &pending.kind,
            PersistKind::Publication { generation: queued, .. } if *queued == generation
        )
}

fn fail_persistence(app: &mut App, pending: Persistence, error: String) {
    match pending.kind {
        PersistKind::Definition { generation, .. } => {
            app.finish_command_enrichment_save(
                generation,
                &pending.view,
                pending.revision,
                Err(error),
            );
        }
        PersistKind::Publication { generation, .. } => {
            app.finish_command_enrichment_run(
                generation,
                &pending.view,
                pending.revision,
                Err(format!("Previous published results retained: {error}")),
            );
        }
    }
}

fn native_fingerprint(native: &[lvu::EnrichmentDefinition]) -> String {
    let mut encoded = Vec::new();
    for definition in native {
        for value in [&definition.id.0, &definition.source] {
            encoded.extend_from_slice(&(value.len() as u64).to_le_bytes());
            encoded.extend_from_slice(value.as_bytes());
        }
    }
    Uuid::new_v5(&SCOPE_NAMESPACE, &encoded).to_string()
}

fn command_fence(app: &App, view: &str, revision: u64, fingerprint: &str) -> bool {
    app.persistent_view_state(view).is_some_and(|state| {
        state.command_enrichment_revision == revision
            && native_fingerprint(&state.applied_enrichments) == fingerprint
    })
}

fn dialog_running(app: &App, generation: u64, view: &str, revision: u64) -> bool {
    app.layers.external_command.state().is_some_and(|dialog| {
        dialog.generation == generation
            && dialog.view_id == view
            && dialog.base_definition_revision == revision
            && dialog.run_state == CommandEnrichmentRunState::Running
    })
}

fn bounded(mut value: String, maximum: usize) -> String {
    if value.len() > maximum {
        let mut end = maximum;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        value.truncate(end);
    }
    value
}

fn normalize_stage(cwd: &Path, stage: &mut CommandEnrichmentStage) -> Result<(), String> {
    if stage.definition.restart != lvu_core::RestartPolicy::Never {
        return Err("command enrichment cannot use a restart policy".into());
    }
    let selected = stage
        .definition
        .cwd
        .take()
        .unwrap_or_else(|| cwd.to_path_buf());
    let absolute = if selected.is_absolute() {
        selected
    } else {
        cwd.join(selected)
    };
    let normalized =
        std::fs::canonicalize(&absolute).map_err(|e| format!("command working directory: {e}"))?;
    let metadata =
        std::fs::metadata(&normalized).map_err(|e| format!("command working directory: {e}"))?;
    if !metadata.is_dir() {
        return Err("command working directory is not a directory".into());
    }
    stage.definition.cwd = Some(normalized);
    Ok(())
}

fn command_scope(
    view: &str,
    stage: &CommandEnrichmentStage,
    native: &[lvu::EnrichmentDefinition],
) -> Result<CommandAttemptScope, String> {
    let view_id = ViewId(Uuid::parse_str(view).map_err(|e| format!("invalid view identity: {e}"))?);
    let command = serde_json::to_vec(&stage.definition).map_err(|e| e.to_string())?;
    let mut preceding = Vec::new();
    for definition in native {
        for value in [&definition.id.0, &definition.source] {
            preceding.extend_from_slice(&(value.len() as u64).to_le_bytes());
            preceding.extend_from_slice(value.as_bytes());
        }
    }
    Ok(CommandAttemptScope {
        view_id,
        stage_id: stage.id.0.clone(),
        command_revision: Uuid::new_v5(&SCOPE_NAMESPACE, &command).to_string(),
        preceding_definition_revision: Uuid::new_v5(&SCOPE_NAMESPACE, &preceding).to_string(),
    })
}

fn review(prepared: &command_snapshot::PreparedCommand, token: String) -> CommandEnrichmentReview {
    let (executable, arguments) = match &prepared.definition.program {
        CommandProgram::Shell { text } => ("shell".into(), vec![text.clone()]),
        CommandProgram::Exec { executable, args } => {
            (executable.display().to_string(), args.clone())
        }
    };
    CommandEnrichmentReview {
        review_token: token,
        record_count: usize::try_from(prepared.stats.output_records)
            .unwrap_or(prepared.events.len()),
        source_count: prepared.summary.sources.len(),
        executable,
        arguments,
        cwd: prepared
            .definition
            .cwd
            .as_ref()
            .map(|p| p.display().to_string()),
        environment_keys: prepared.definition.environment.keys().cloned().collect(),
    }
}

pub(super) fn clear_cloned_publication(state: &mut lvu::PersistentViewState) -> bool {
    let copied = state.command_enrichment.is_some();
    state.command_publication = None;
    copied
}

pub(super) fn recipe_command_guard(state: &lvu::PersistentViewState) -> Result<(), &'static str> {
    if state.command_enrichment.is_some() {
        Err("command steps are saved with working views; recipe support is not available yet")
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lvu::EnrichmentStageId;
    use lvu::app::CommandEnrichmentStageId;
    use lvu_core::CommandDefinition;
    use std::collections::BTreeMap;

    fn stage(cwd: Option<PathBuf>) -> CommandEnrichmentStage {
        CommandEnrichmentStage {
            id: CommandEnrichmentStageId("command".into()),
            definition: CommandDefinition {
                program: CommandProgram::Exec {
                    executable: "tool".into(),
                    args: vec!["a".into()],
                },
                cwd,
                environment: BTreeMap::new(),
                restart: lvu_core::RestartPolicy::Never,
            },
        }
    }

    #[test]
    fn scope_ignores_live_data_but_changes_with_command_and_native_prefix() {
        let stage = stage(Some("/tmp".into()));
        let view = Uuid::new_v4().to_string();
        let first = command_scope(&view, &stage, &[]).unwrap();
        assert_eq!(first, command_scope(&view, &stage, &[]).unwrap());
        let native = vec![lvu::EnrichmentDefinition {
            id: EnrichmentStageId("n".into()),
            source: "x = pl.lit(1)".into(),
        }];
        assert_ne!(
            first.preceding_definition_revision,
            command_scope(&view, &stage, &native)
                .unwrap()
                .preceding_definition_revision
        );
        let mut changed = stage;
        if let CommandProgram::Exec { args, .. } = &mut changed.definition.program {
            args.push("b".into());
        }
        assert_ne!(
            first.command_revision,
            command_scope(&view, &changed, &[])
                .unwrap()
                .command_revision
        );
    }

    #[test]
    fn save_keeps_exact_blank_cwd_while_prepare_resolves_it_on_worker_copy() {
        let saved = stage(None);
        let mut prepared = saved.clone();
        normalize_stage(Path::new("/tmp"), &mut prepared).unwrap();
        assert_eq!(saved.definition.cwd, None);
        assert_eq!(prepared.definition.cwd, Some(PathBuf::from("/tmp")));
    }

    #[test]
    fn definition_patch_preserves_last_good_publication() {
        let mut state = lvu::PersistentViewState {
            command_enrichment: Some(stage(Some("/old".into()))),
            command_enrichment_revision: 7,
            command_publication: Some("last-good".into()),
            ..Default::default()
        };
        let pending = Persistence {
            view: Uuid::new_v4().to_string(),
            revision: 8,
            sequence: None,
            request: None,
            kind: PersistKind::Definition {
                generation: 3,
                stage: Some(stage(None)),
            },
        };
        apply_persistence_patch(&mut state, &pending).unwrap();
        assert_eq!(state.command_enrichment_revision, 8);
        assert_eq!(state.command_enrichment.unwrap().definition.cwd, None);
        assert_eq!(state.command_publication.as_deref(), Some("last-good"));
    }

    #[test]
    fn publication_patch_rejects_a_changed_native_prefix() {
        let mut state = lvu::PersistentViewState {
            command_enrichment_revision: 8,
            command_publication: Some("last-good".into()),
            ..Default::default()
        };
        let pending = Persistence {
            view: Uuid::new_v4().to_string(),
            revision: 8,
            sequence: None,
            request: None,
            kind: PersistKind::Publication {
                generation: 3,
                native_fingerprint: native_fingerprint(&[lvu::EnrichmentDefinition {
                    id: EnrichmentStageId("native".into()),
                    source: "value = pl.col('raw')".into(),
                }]),
                reference: "new".into(),
                rows: Default::default(),
                record_count: 0,
            },
        };
        assert!(apply_persistence_patch(&mut state, &pending).is_err());
        assert_eq!(state.command_publication.as_deref(), Some("last-good"));
    }

    #[test]
    fn only_pre_dispatch_publication_matches_cancel() {
        let view = Uuid::new_v4().to_string();
        let mut pending = Persistence {
            view: view.clone(),
            revision: 8,
            sequence: None,
            request: None,
            kind: PersistKind::Publication {
                generation: 3,
                native_fingerprint: native_fingerprint(&[]),
                reference: "new".into(),
                rows: Default::default(),
                record_count: 0,
            },
        };
        assert!(queued_publication_matches(&pending, 3, &view));
        assert!(!queued_publication_matches(&pending, 4, &view));
        pending.sequence = Some(19);
        assert!(!queued_publication_matches(&pending, 3, &view));
    }

    #[test]
    fn accepted_publication_ack_attaches_after_dialog_closes() {
        let view = Uuid::new_v4().to_string();
        let source = Uuid::new_v4().to_string();
        let mut app = App::new(
            Vec::new(),
            vec![lvu::ViewItem {
                id: view.clone(),
                source_id: source,
                name: "events".into(),
            }],
            false,
        );
        app.restore_persistent_view(
            &view,
            lvu::PersistentViewState {
                command_enrichment: Some(stage(None)),
                command_enrichment_revision: 8,
                ..Default::default()
            },
        );
        assert!(app.layers.external_command.state().is_none());
        assert!(app.commit_command_publication(&view, 8, "accepted".into()));
        assert_eq!(
            app.persistent_view_state(&view)
                .and_then(|state| state.command_publication),
            Some("accepted".into())
        );
    }

    #[test]
    fn clone_and_recipe_guards_keep_definition_inert_without_foreign_results() {
        let mut state = lvu::PersistentViewState {
            command_enrichment: Some(CommandEnrichmentStage {
                id: CommandEnrichmentStageId("command".into()),
                definition: CommandDefinition {
                    program: CommandProgram::Shell {
                        text: "tool".into(),
                    },
                    cwd: Some("/tmp".into()),
                    environment: BTreeMap::new(),
                    restart: lvu_core::RestartPolicy::Never,
                },
            }),
            command_enrichment_revision: 4,
            command_publication: Some("foreign-view-reference".into()),
            ..Default::default()
        };
        assert!(recipe_command_guard(&state).is_err());
        assert!(clear_cloned_publication(&mut state));
        assert!(state.command_enrichment.is_some());
        assert_eq!(state.command_enrichment_revision, 4);
        assert_eq!(state.command_publication, None);
    }
}
