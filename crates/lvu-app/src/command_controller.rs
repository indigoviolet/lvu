//! Explicit command-enrichment orchestration. Blocking work never runs on the UI tick.
//!
//! A command step is one ordered step of a view's enrichment chain
//! (docs/command-enrichment.md). Saving it is a chain change the query seam
//! accepts; this controller owns only the explicit run: freezing the input
//! the steps before it produce, the bounded review, execution, the durable
//! publication of results per step, restoring saved publications, and
//! handing published rows to `lvu-view` as columns so later steps read them.

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
    collections::{HashMap, HashSet, VecDeque},
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

/// Column updates queued for `lvu-view` while no adapter is at hand.
const MAX_COLUMN_UPDATES: usize = 256;

pub(super) struct CommandController {
    workspace: PathBuf,
    cwd: PathBuf,
    presentation: command_rows::CommandPresentation,
    active: Option<Active>,
    persistence: Option<Persistence>,
    /// `(view, stage)` → the publication reference last handed to the view.
    observed: HashMap<(String, String), Option<String>>,
    restore_queue: VecDeque<RestoreRequest>,
    /// Published rows `lvu-view` has not been given as columns yet.
    column_updates: VecDeque<ColumnUpdate>,
    /// Views whose chain must be re-evaluated over new command columns once
    /// nothing of theirs is in flight.
    reaffirm_pending: HashSet<String>,
}

enum Active {
    Preparing {
        generation: u64,
        view: String,
        stage: String,
        revision: u64,
        native_fingerprint: String,
        cancel: Arc<AtomicBool>,
        rx: mpsc::Receiver<Result<command_snapshot::PreparedCommand, String>>,
        worker: JoinHandle<()>,
    },
    Ready {
        generation: u64,
        view: String,
        stage: String,
        revision: u64,
        native_fingerprint: String,
        token: String,
        prepared: command_snapshot::PreparedCommand,
    },
    Executing {
        generation: u64,
        view: String,
        stage: String,
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
    stage: String,
    name: String,
    reference: String,
}

struct ColumnUpdate {
    view: String,
    stage: String,
    name: String,
    /// `None` clears the step's columns.
    rows: Option<command_rows::CommandResults>,
}

enum PersistKind {
    Publication {
        generation: u64,
        stage: String,
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
            column_updates: VecDeque::new(),
            reaffirm_pending: HashSet::new(),
        }
    }

    fn queue_columns(
        &mut self,
        view: &str,
        stage: &str,
        name: &str,
        rows: Option<command_rows::CommandResults>,
    ) {
        // Newest update for a step wins; the queue never grows past a bound.
        self.column_updates
            .retain(|update| !(update.view == view && update.stage == stage));
        if self.column_updates.len() >= MAX_COLUMN_UPDATES {
            self.column_updates.pop_front();
        }
        self.column_updates.push_back(ColumnUpdate {
            view: view.to_owned(),
            stage: stage.to_owned(),
            name: name.to_owned(),
            rows,
        });
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
                    let Some((position, stage)) = state
                        .applied_enrichments
                        .iter()
                        .position(|step| step.id.0 == stage_id.0 && step.is_command())
                        .and_then(|position| {
                            state.applied_enrichments[position]
                                .command_stage()
                                .map(|stage| (position, stage))
                        })
                        .filter(|_| {
                            state
                                .command_steps
                                .get(&stage_id.0)
                                .is_some_and(|run| run.revision == definition_revision)
                        })
                    else {
                        app.finish_command_enrichment_review(
                            generation,
                            &view_id,
                            definition_revision,
                            Err("saved command definition is stale".into()),
                        );
                        continue;
                    };
                    let prefix = state.applied_enrichments[..position].to_vec();
                    let native_fingerprint = native_fingerprint(&prefix);
                    let frozen = match adapter.freeze_input_through(
                        &view_id,
                        Some(&stage_id.0),
                        command_snapshot::input_limits(),
                    ) {
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
                    let worker_view = view_id.clone();
                    let (tx, rx) = mpsc::sync_channel(1);
                    let worker = std::thread::spawn(move || {
                        let result = (|| {
                            let mut effective = stage;
                            normalize_stage(&cwd, &mut effective)?;
                            let scope = command_scope(&worker_view, &effective, &prefix)?;
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
                        stage: stage_id.0,
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
                    stage_id,
                    review_token,
                } => {
                    let ready = self.command_controller.active.take();
                    let Some(Active::Ready {
                        generation: g,
                        view,
                        stage,
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
                    if (g, view.as_str(), stage.as_str(), revision, token.as_str())
                        != (
                            generation,
                            view_id.as_str(),
                            stage_id.0.as_str(),
                            definition_revision,
                            review_token.as_str(),
                        )
                        || !command_fence(app, &view, &stage, revision, &native_fingerprint)
                    {
                        self.command_controller.active = Some(Active::Ready {
                            generation: g,
                            view,
                            stage,
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
                        stage,
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
        changed |= self.flush_command_columns(app, adapter);
        changed
    }

    /// Hands published rows to `lvu-view` as `<name>.<field>` columns and,
    /// once the view has nothing in flight, re-evaluates its chain over them
    /// (§12.5 downstream queries). A view with a pending query waits: the
    /// reaffirmation must not replace a change the user is still making.
    fn flush_command_columns(&mut self, app: &mut App, adapter: &NativeViewAdapter) -> bool {
        let mut changed = false;
        while let Some(update) = self.command_controller.column_updates.pop_front() {
            let applied = match update.rows {
                Some(rows) => adapter.set_command_results(
                    &update.view,
                    &update.stage,
                    &update.name,
                    rows.into_iter()
                        .map(|(id, result)| {
                            ((id.source_id.0.to_string(), id.sequence), result.fields)
                        })
                        .collect(),
                ),
                None => adapter.clear_command_results(&update.view, &update.stage),
            };
            if applied {
                self.command_controller.reaffirm_pending.insert(update.view);
                changed = true;
            }
        }
        let due: Vec<String> = self
            .command_controller
            .reaffirm_pending
            .iter()
            .filter(|view| !app.view_has_pending_query(view))
            .cloned()
            .collect();
        for view in due {
            self.command_controller.reaffirm_pending.remove(&view);
            if app.views.reaffirm_enrichment_chain(&view).is_some() {
                changed = true;
            }
        }
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
                stage,
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
                                && command_fence(app, &view, &stage, revision, &native_fingerprint)
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
                                    stage,
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
                        stage,
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
                stage,
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
                                && command_fence(
                                    app,
                                    &view,
                                    &stage,
                                    revision,
                                    &native_fingerprint,
                                )
                                && dialog_running(app, generation, &view, &stage, revision) =>
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
                                .can_publish(&view, &stage, &rows)
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
                                            stage,
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
                        stage,
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
                            state
                                .command_steps
                                .get(&request.stage)
                                .and_then(|run| run.publication.as_deref())
                                == Some(&request.reference)
                        })
                        && let Ok(rows) = result
                    {
                        match self.command_controller.presentation.publish(
                            &request.view,
                            &request.stage,
                            rows.clone(),
                        ) {
                            Ok(()) => self.command_controller.queue_columns(
                                &request.view,
                                &request.stage,
                                &request.name,
                                Some(rows),
                            ),
                            Err(error) => {
                                app.action_notice = Some(bounded(
                                    format!("Saved command results unavailable: {error}"),
                                    1024,
                                ));
                            }
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
                stage,
                revision,
                native_fingerprint,
                token,
                ..
            }) => {
                !command_fence(app, view, stage, *revision, native_fingerprint)
                    || !app.layers.external_command.state().is_some_and(|dialog| {
                        dialog.generation == *generation
                            && dialog.view_id == *view
                            && dialog.stage_id == *stage
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
        // The command steps of every open view, in chain order.
        let steps: Vec<(String, Vec<command_rows::CommandStepKey>)> = app
            .views()
            .iter()
            .filter_map(|view| {
                app.persistent_view_state(&view.id).map(|state| {
                    (
                        view.id.clone(),
                        state
                            .applied_enrichments
                            .iter()
                            .filter(|step| step.is_command())
                            .map(|step| command_rows::CommandStepKey {
                                stage: step.id.0.clone(),
                                name: step.source.clone(),
                            })
                            .collect(),
                    )
                })
            })
            .collect();
        let is_step = |view: &str, stage: &str| {
            steps
                .iter()
                .any(|(id, keys)| id == view && keys.iter().any(|key| key.stage == stage))
        };
        // Steps that left a chain take their columns with them.
        let gone: Vec<(String, String)> = self
            .command_controller
            .observed
            .keys()
            .filter(|(view, stage)| !is_step(view, stage))
            .cloned()
            .collect();
        for (view, stage) in gone {
            self.command_controller
                .observed
                .remove(&(view.clone(), stage.clone()));
            if open.contains(view.as_str()) {
                self.command_controller
                    .queue_columns(&view, &stage, "", None);
            }
        }
        self.command_controller.restore_queue.retain(|request| {
            is_step(&request.view, &request.stage)
                && app
                    .persistent_view_state(&request.view)
                    .is_some_and(|state| {
                        state
                            .command_steps
                            .get(&request.stage)
                            .and_then(|run| run.publication.as_deref())
                            == Some(request.reference.as_str())
                    })
        });
        self.command_controller
            .presentation
            .configure(steps.iter().cloned());
        let mut restore_queue_full = false;
        for (view, keys) in &steps {
            let Some(state) = app.persistent_view_state(view) else {
                continue;
            };
            for key in keys {
                let reference = state
                    .command_steps
                    .get(&key.stage)
                    .and_then(|run| run.publication.clone());
                let observed_key = (view.clone(), key.stage.clone());
                // Last-good publication is independent of later definition
                // revisions. Reload only when its immutable reference changes.
                if self.command_controller.observed.get(&observed_key) == Some(&reference) {
                    continue;
                }
                if let Some(reference) = reference {
                    let duplicate = self.command_controller.restore_queue.iter().any(|queued| {
                        queued.view == *view
                            && queued.stage == key.stage
                            && queued.reference == reference
                    });
                    if duplicate {
                        self.command_controller
                            .observed
                            .insert(observed_key, Some(reference));
                    } else if self.command_controller.restore_queue.len() < 128 {
                        self.command_controller
                            .restore_queue
                            .push_back(RestoreRequest {
                                view: view.clone(),
                                stage: key.stage.clone(),
                                name: key.name.clone(),
                                reference: reference.clone(),
                            });
                        self.command_controller
                            .observed
                            .insert(observed_key, Some(reference));
                    } else {
                        restore_queue_full = true;
                    }
                } else {
                    self.command_controller.observed.insert(observed_key, None);
                    self.command_controller.presentation.clear(view, &key.stage);
                    self.command_controller
                        .queue_columns(view, &key.stage, &key.name, None);
                }
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
        let PersistKind::Publication {
            generation,
            stage,
            native_fingerprint,
            ..
        } = &pending.kind;
        if !dialog_running(app, *generation, &pending.view, stage, pending.revision)
            || !command_fence(
                app,
                &pending.view,
                stage,
                pending.revision,
                native_fingerprint,
            )
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
                let PersistKind::Publication { generation, .. } = &pending.kind;
                let entered =
                    app.begin_command_result_save(*generation, &pending.view, pending.revision);
                debug_assert!(
                    entered,
                    "publication commit point must match running dialog"
                );
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
            (
                MemoryEvent::Saved(..),
                PersistKind::Publication {
                    generation,
                    stage,
                    native_fingerprint: _,
                    reference,
                    rows,
                    record_count,
                },
            ) => {
                if app.commit_command_publication(
                    &pending.view,
                    &stage,
                    pending.revision,
                    reference.clone(),
                ) {
                    self.command_controller
                        .observed
                        .insert((pending.view.clone(), stage.clone()), Some(reference));
                    let name = app
                        .persistent_view_state(&pending.view)
                        .and_then(|state| {
                            state
                                .applied_enrichments
                                .iter()
                                .find(|step| step.id.0 == stage)
                                .map(|step| step.source.clone())
                        })
                        .unwrap_or_else(|| lvu::app::DEFAULT_COMMAND_STEP_NAME.to_owned());
                    // Controller publication is globally serialized, so nothing can
                    // invalidate can_publish admission before this matching publish.
                    match self.command_controller.presentation.publish(
                        &pending.view,
                        &stage,
                        rows.clone(),
                    ) {
                        Ok(()) => {
                            self.command_controller.queue_columns(
                                &pending.view,
                                &stage,
                                &name,
                                Some(rows),
                            );
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
    let PersistKind::Publication {
        stage,
        native_fingerprint: expected_fingerprint,
        reference,
        ..
    } = &pending.kind;
    let Some(prefix) = chain_prefix(&state.applied_enrichments, stage) else {
        return Err("command step left the chain before publication".into());
    };
    if state
        .command_steps
        .get(stage)
        .is_none_or(|run| run.revision != pending.revision)
        || native_fingerprint(prefix) != *expected_fingerprint
    {
        return Err("command definition changed before publication".into());
    }
    state
        .command_steps
        .entry(stage.clone())
        .or_default()
        .publication = Some(reference.clone());
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
    let PersistKind::Publication { generation, .. } = pending.kind;
    app.finish_command_enrichment_run(
        generation,
        &pending.view,
        pending.revision,
        Err(format!("Previous published results retained: {error}")),
    );
}

/// The steps before `stage` in `chain`: what the command reads.
fn chain_prefix<'a>(
    chain: &'a [lvu::EnrichmentDefinition],
    stage: &str,
) -> Option<&'a [lvu::EnrichmentDefinition]> {
    chain
        .iter()
        .position(|step| step.id.0 == stage && step.is_command())
        .map(|position| &chain[..position])
}

/// Identity of a chain prefix: every step's id and source, and a command
/// step's whole definition. A run's input is these steps' output, so a change
/// to any of them fences the run (docs/command-enrichment.md).
fn native_fingerprint(prefix: &[lvu::EnrichmentDefinition]) -> String {
    Uuid::new_v5(&SCOPE_NAMESPACE, &prefix_bytes(prefix)).to_string()
}

fn prefix_bytes(prefix: &[lvu::EnrichmentDefinition]) -> Vec<u8> {
    let mut encoded = Vec::new();
    for definition in prefix {
        let command = definition
            .command
            .as_ref()
            .map(|command| serde_json::to_vec(command).unwrap_or_default())
            .unwrap_or_default();
        for value in [
            definition.id.0.as_bytes(),
            definition.source.as_bytes(),
            command.as_slice(),
        ] {
            encoded.extend_from_slice(&(value.len() as u64).to_le_bytes());
            encoded.extend_from_slice(value);
        }
    }
    encoded
}

fn command_fence(app: &App, view: &str, stage: &str, revision: u64, fingerprint: &str) -> bool {
    app.persistent_view_state(view).is_some_and(|state| {
        state
            .command_steps
            .get(stage)
            .is_some_and(|run| run.revision == revision)
            && chain_prefix(&state.applied_enrichments, stage)
                .is_some_and(|prefix| native_fingerprint(prefix) == fingerprint)
    })
}

fn dialog_running(app: &App, generation: u64, view: &str, stage: &str, revision: u64) -> bool {
    app.layers.external_command.state().is_some_and(|dialog| {
        dialog.generation == generation
            && dialog.view_id == view
            && dialog.stage_id == stage
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

/// The durable attempt scope: the view, the step, the command definition and
/// the chain prefix it reads. Two runs with the same scope over the same
/// records are the same attempt.
fn command_scope(
    view: &str,
    stage: &CommandEnrichmentStage,
    prefix: &[lvu::EnrichmentDefinition],
) -> Result<CommandAttemptScope, String> {
    let view_id = ViewId(Uuid::parse_str(view).map_err(|e| format!("invalid view identity: {e}"))?);
    let command = serde_json::to_vec(&stage.definition).map_err(|e| e.to_string())?;
    Ok(CommandAttemptScope {
        view_id,
        stage_id: stage.id.0.clone(),
        command_revision: Uuid::new_v5(&SCOPE_NAMESPACE, &command).to_string(),
        preceding_definition_revision: native_fingerprint(prefix),
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

/// A cloned view keeps its command definitions and none of their results:
/// a publication belongs to the view whose input it was computed from.
/// Returns whether any command step was copied.
pub(super) fn clear_cloned_publication(state: &mut lvu::PersistentViewState) -> bool {
    for run in state.command_steps.values_mut() {
        run.publication = None;
    }
    state
        .applied_enrichments
        .iter()
        .any(lvu::EnrichmentDefinition::is_command)
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
            id: CommandEnrichmentStageId("command-1".into()),
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

    fn chain_with(stage: &CommandEnrichmentStage) -> Vec<lvu::EnrichmentDefinition> {
        vec![
            lvu::EnrichmentDefinition {
                id: EnrichmentStageId("native".into()),
                source: "value = pl.col('raw')".into(),
                command: None,
            },
            lvu::EnrichmentDefinition::command(
                stage.id.0.clone(),
                String::from("command"),
                stage.definition.clone(),
            ),
        ]
    }

    #[test]
    fn scope_ignores_live_data_but_changes_with_command_and_chain_prefix() {
        let stage = stage(Some("/tmp".into()));
        let view = Uuid::new_v4().to_string();
        let first = command_scope(&view, &stage, &[]).unwrap();
        assert_eq!(first, command_scope(&view, &stage, &[]).unwrap());
        let native = vec![lvu::EnrichmentDefinition {
            id: EnrichmentStageId("n".into()),
            source: "x = pl.lit(1)".into(),
            command: None,
        }];
        assert_ne!(
            first.preceding_definition_revision,
            command_scope(&view, &stage, &native)
                .unwrap()
                .preceding_definition_revision
        );
        // An earlier command step is part of the prefix too: its definition
        // changing changes what this one reads.
        let mut earlier = lvu::EnrichmentDefinition::command(
            String::from("command-0"),
            String::from("first"),
            stage.definition.clone(),
        );
        let with_earlier = command_scope(&view, &stage, std::slice::from_ref(&earlier)).unwrap();
        if let Some(command) = &mut earlier.command
            && let CommandProgram::Exec { args, .. } = &mut command.program
        {
            args.push("--changed".into());
        }
        assert_ne!(
            with_earlier.preceding_definition_revision,
            command_scope(&view, &stage, std::slice::from_ref(&earlier))
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
    fn publication_patch_rejects_a_changed_prefix_or_revision() {
        let stage = stage(None);
        let chain = chain_with(&stage);
        let mut state = lvu::PersistentViewState {
            applied_enrichments: chain.clone(),
            command_steps: BTreeMap::from([(
                "command-1".to_owned(),
                lvu::app::CommandStepState {
                    revision: 8,
                    publication: Some("last-good".into()),
                },
            )]),
            ..Default::default()
        };
        let pending = |revision: u64, fingerprint: String| Persistence {
            view: Uuid::new_v4().to_string(),
            revision,
            sequence: None,
            request: None,
            kind: PersistKind::Publication {
                generation: 3,
                stage: "command-1".into(),
                native_fingerprint: fingerprint,
                reference: "new".into(),
                rows: Default::default(),
                record_count: 0,
            },
        };
        // A prefix that is not the chain's: refused, last-good kept.
        assert!(apply_persistence_patch(&mut state, &pending(8, native_fingerprint(&[]))).is_err());
        // A revision that is not the step's: refused.
        assert!(
            apply_persistence_patch(&mut state, &pending(9, native_fingerprint(&chain[..1])))
                .is_err()
        );
        assert_eq!(
            state.command_steps["command-1"].publication.as_deref(),
            Some("last-good")
        );
        apply_persistence_patch(&mut state, &pending(8, native_fingerprint(&chain[..1]))).unwrap();
        assert_eq!(
            state.command_steps["command-1"].publication.as_deref(),
            Some("new")
        );
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
                stage: "command-1".into(),
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
        let stage = stage(None);
        app.restore_persistent_view(
            &view,
            lvu::PersistentViewState {
                applied_enrichments: chain_with(&stage),
                command_steps: BTreeMap::from([(
                    "command-1".to_owned(),
                    lvu::app::CommandStepState {
                        revision: 8,
                        publication: None,
                    },
                )]),
                ..Default::default()
            },
        );
        // The restored chain is applied through the query seam like any
        // other; the step keeps the revision it was saved with.
        let request = app.take_query_requests().pop().expect("restore query");
        assert!(app.apply_query_completion(lvu::QueryCompletion {
            view_id: request.view_id,
            generation: request.generation,
            revision: request.revision,
            purpose: request.purpose,
            result: Ok(()),
        }));
        assert_eq!(
            app.persistent_view_state(&view).unwrap().command_steps["command-1"].revision,
            8
        );
        assert!(app.layers.external_command.state().is_none());
        assert!(!app.commit_command_publication(&view, "command-1", 7, "stale".into()));
        assert!(!app.commit_command_publication(&view, "command-2", 8, "other".into()));
        assert!(app.commit_command_publication(&view, "command-1", 8, "accepted".into()));
        assert_eq!(
            app.persistent_view_state(&view)
                .and_then(|state| state.command_steps["command-1"].publication.clone()),
            Some("accepted".into())
        );
    }

    #[test]
    fn a_clone_keeps_command_definitions_and_drops_their_results() {
        let stage = stage(Some("/tmp".into()));
        let mut state = lvu::PersistentViewState {
            applied_enrichments: chain_with(&stage),
            command_steps: BTreeMap::from([(
                "command-1".to_owned(),
                lvu::app::CommandStepState {
                    revision: 4,
                    publication: Some("foreign-view-reference".into()),
                },
            )]),
            ..Default::default()
        };
        assert!(clear_cloned_publication(&mut state));
        assert_eq!(state.applied_enrichments.len(), 2);
        assert_eq!(state.command_steps["command-1"].revision, 4);
        assert_eq!(state.command_steps["command-1"].publication, None);
        let mut plain = lvu::PersistentViewState::default();
        assert!(!clear_cloned_publication(&mut plain));
    }
}
