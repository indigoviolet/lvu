//! Foreground shared-capture wiring: spawn/attach, remote control, and
//! mediated save/drain through the background worker.
//!
//! This module is the WINDOW side of shared mode. Capture itself stays in
//! the worker (`lvu-shared`); live reads stay behind the remote handle seam
//! (fc429's paths/signatures plug in at [`RemoteSource`]: it carries exactly
//! what an adapter needs — the worker-owned identity plus the journal path
//! — while [`MemoryEvent`] outputs flow to existing consumers unchanged).
//!
//! Nothing here remodels: requests decompose 1:1 into `StoreMethod`, replies
//! map 1:1 back onto the app's own [`MemoryEvent`], and sequences travel
//! verbatim (process-local, never global). Conversions reuse the existing
//! `memory::working_view` and field-identical DTO projections; the day union
//! adds the two `Serialize` derives those projections collapse (same note as
//! the protocol docs).
//!
//! Single-writer rule: shared mode starts NO local `MemoryWorker`, so the
//! background worker is the only durable-view writer; there is no local
//! fallback that could split-brain view/recipe state. One known exception:
//! command-enrichment EXECUTION threads (`command_controller`) open
//! short-lived sqlite handles for attempt reservation/delivery. SQLite
//! locking serializes those against the worker (busy errors surface
//! loudly, never silent corruption), but concurrent enrichment under
//! sharing can hit contention that local mode never sees — mediating
//! execution is follow-up work, not this module.
//!
//! Transport-complete but session-unconsumed until the handle seam lands:
//! `startup` drives the lifecycle, while the per-source and per-save entry
//! points below wait for the `StartedSource`/controller cutover. The allow
//! lifts with that cutover; until then an uncalled function here is pending
//! wiring, never dead design.
#![allow(dead_code)]

//! Version tracking mirrors the local worker thread's `versions` map via
//! [`SaveBases`](lvu_shared::SaveBases): the last committed version per
//! view travels as the next save's base, loads reseed every returned view,
//! derived creation seeds the echoed version, and a conflict keeps the
//! last-success base (never adopts the peer's). Recovery is reload (which
//! reseeds to truth) plus an explicit user merge — the merge UX itself is
//! controller work at the cutover and is NOT claimed to exist here; what
//! exists is the mechanism that makes it converge instead of conflicting
//! forever, plus the guarantee that automatic saves after a conflict keep
//! failing loudly rather than overwriting the peer.
//!
//! Flush needs no failure memory here, unlike the local worker thread: local
//! saves are fire-and-forget (failures surface later via poll, so flush must
//! remember them), while every remote save answers synchronously — every
//! failure is already in the caller's hands before flush runs.
//!
//! Status: spawn/attach/control/save/drain transport is complete. Session
//! consumption (`StartedSource` abstraction, controller cutover) waits for
//! the handle seam. Stdin capture stays window-local (no chunk-driving
//! client exists, so a remote stdin start is refused explicitly rather than
//! hung) and HTTP stays refused, mirroring the runtime's supported set.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use lvu::{RecipeRequestMeta, app::RecipeOutcome};
use lvu_core::{SourceDefinition, SourceId, ViewId};
use lvu_shared::{
    SaveBases, StartOutcome, StoreEvent, StoreMethod, SuggestionContextShape,
    SuggestionOutcomeShape, WorkerClient,
};

use crate::memory::{Event as MemoryEvent, SaveRequest, SuggestionContext};

/// A worker-owned capture, ready for adapter input: the identity the worker
/// enforces plus the journal path it derived (windows never hardcode the
/// capture layout).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteSource {
    pub source_id: SourceId,
    pub journal_path: PathBuf,
}

/// An acquired worker-owned capture with its live read handle: `remote`
/// is the adapter input (identity + journal path for registration and
/// session restore), `handle` drives live/query reads through its own
/// tail plus a feeder-kept progress cache. The feeder task is owned by
/// the session and dies with it (stop/drain aborts).
pub struct SharedSource {
    pub remote: RemoteSource,
    pub handle: lvu_shared::RemoteSourceHandle,
}

/// One source's feeder task plus its cooperative stop flag. The flag
/// (not abort) is the normal exit so an in-flight bounded exchange
/// finishes instead of poisoning the shared client.
type FeederEntry = (
    tokio::task::JoinHandle<()>,
    std::sync::Arc<std::sync::atomic::AtomicBool>,
);

/// One window's shared session: the worker connection (shared with per-
/// source feeder tasks), the compare-and-swap bases (see [`SaveBases`]),
/// the live read handles, and the feeder tasks keeping them fresh.
/// Interior mutability throughout: feeders, handles, store calls, and
/// control share one client without ever aliasing `&mut`, so awaiting one
/// operation never blocks another's borrow.
pub struct SharedStore {
    client: std::sync::Arc<tokio::sync::Mutex<WorkerClient>>,
    bases: std::sync::Mutex<SaveBases>,
    next_request: std::sync::atomic::AtomicU64,
    feeders: std::sync::Mutex<HashMap<SourceId, FeederEntry>>,
    handles: std::sync::Mutex<HashMap<SourceId, lvu_shared::RemoteSourceHandle>>,
}

impl SharedStore {
    /// Spawn-or-attach the background worker for `capture_root` and
    /// handshake. One session per window process: the viewer slot and
    /// handshake use this process id, matching the worker's audience
    /// accounting.
    pub async fn startup(
        executable: &Path,
        capture_root: &Path,
        window_id: &str,
    ) -> Result<Self, String> {
        let (client, _presence) =
            WorkerClient::attach(executable, capture_root, window_id, std::process::id()).await?;
        Ok(Self::from_client(client))
    }

    /// Wrap an existing attachment (tests, or wiring that attached
    /// first and sessions later). The version map starts empty: bases
    /// accrue from saves and loads on this session only.
    pub fn from_client(client: WorkerClient) -> Self {
        Self {
            client: std::sync::Arc::new(tokio::sync::Mutex::new(client)),
            bases: std::sync::Mutex::new(SaveBases::new()),
            next_request: std::sync::atomic::AtomicU64::new(1),
            feeders: std::sync::Mutex::new(HashMap::new()),
            handles: std::sync::Mutex::new(HashMap::new()),
        }
    }

    fn take_request_id(&self) -> String {
        let id = self
            .next_request
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        format!("shared-{id}")
    }

    fn base_for(&self, view_id: ViewId) -> Option<u64> {
        self.bases
            .lock()
            .expect("shared bases poisoned")
            .base_for(view_id)
    }

    /// Explicit user-approved acquisition through the worker. The definition
    /// travels as its canonical DTO; admission (identity dedup) is enforced
    /// worker-side and the live capture is re-presented, never double-started.
    /// On success a feeder task starts keeping the handle's progress cache
    /// fresh on the blocking lane; stopping replaces it (see `stop_source`).
    pub async fn start_source(
        &self,
        definition: &SourceDefinition,
    ) -> Result<SharedSource, String> {
        let started = match self.client.lock().await.request_start(definition).await? {
            StartOutcome::Started {
                source_id,
                journal_path,
                ..
            } => RemoteSource {
                source_id,
                journal_path,
            },
            StartOutcome::StdinBound { .. } => {
                return Err("shared capture cannot drive a forwarded stdin pipe: start stdin sources window-locally".into());
            }
        };
        let handle = self.feed_handle(&started).await?;
        Ok(SharedSource {
            remote: started,
            handle,
        })
    }

    /// Build the live read handle for an acquired source and start its
    /// feeder: one initial poll becomes the trusted cache, then a task
    /// keeps it fresh until stopped. Shared by start and restart paths.
    /// The session retains a clone so stop can publish the terminal
    /// snapshot into the same handle adapters read.
    async fn feed_handle(
        &self,
        remote: &RemoteSource,
    ) -> Result<lvu_shared::RemoteSourceHandle, String> {
        let session = self.session_string().await;
        let initial = self
            .client
            .lock()
            .await
            .poll_progress(remote.source_id)
            .await?;
        let handle = lvu_shared::RemoteSourceHandle::new(
            remote.source_id,
            &remote.journal_path,
            session.clone(),
            initial,
            lvu_shared::RemoteConfig::default(),
        )?;
        let fed = handle.clone();
        let feeding = Arc::clone(&self.client);
        // One serve per task, never a silent retry loop: a transport
        // fault ends the feeder (observable via `feeder_alive`), because
        // retrying a retired client could never succeed and spinning cheap
        // failures would mask the outage. Recovery is session re-attach,
        // which restarts feeders against the new connection. The stop flag
        // (not abort) is the normal exit: it lets the in-flight bounded
        // exchange finish so the shared client is never poisoned by us.
        // The serve result is intentionally dropped: task end IS the signal.
        let interval = lvu_shared::DEFAULT_FEED_INTERVAL;
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let task = tokio::spawn(async move {
            let _ = lvu_shared::serve_feed(feeding, fed, session, interval, stopping).await;
        });
        self.replace_feeder(remote.source_id, task, stop);
        self.handles
            .lock()
            .expect("shared handles poisoned")
            .insert(remote.source_id, handle.clone());
        Ok(handle)
    }

    /// This session's worker lifetime nonce, sampled from the handshake.
    async fn session_string(&self) -> String {
        self.client.lock().await.worker_session().to_owned()
    }

    /// Replace (stopping any previous) feeder task for one source. The
    /// previous task is signalled, not aborted: aborting mid-poll would
    /// poison the shared client under retire-on-fault, failing the very
    /// next operation. Signalled tasks exit at their next exchange
    /// boundary; abandonment is impossible because every exchange is
    /// bounded.
    fn replace_feeder(
        &self,
        source_id: SourceId,
        task: tokio::task::JoinHandle<()>,
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) {
        let mut feeders = self.feeders.lock().expect("shared feeders poisoned");
        if let Some((_, previous_stop)) = feeders.insert(source_id, (task, stop)) {
            previous_stop.store(true, std::sync::atomic::Ordering::Release);
        }
    }

    /// Bound for a signalled feeder to reach its exchange boundary. Covers
    /// one poll bound plus publish margin; expiry falls back to abort
    /// (which may retire the shared client — the caller then reports
    /// unknown rather than pretending the stop landed cleanly).
    const FEEDER_STOP_TIMEOUT: Duration = Duration::from_secs(8);

    /// Cooperatively stop one source's feeder: signal it, then await its
    /// task through at most one bounded in-flight exchange. Returns true
    /// when the task exited on its own (client provably clean); on timeout
    /// the task is aborted and false returns, meaning the next client use
    /// may report a retired transport instead of a clean result.
    async fn stop_feeder(&self, source_id: SourceId) -> bool {
        let entry = self
            .feeders
            .lock()
            .expect("shared feeders poisoned")
            .remove(&source_id);
        let Some((mut task, stop)) = entry else {
            return true;
        };
        stop.store(true, std::sync::atomic::Ordering::Release);
        // Await the task itself (not a timeout around it): on expiry the
        // handle is still needed to abort, and `select!` keeps ownership
        // for exactly that.
        tokio::select! {
            result = &mut task => result.is_ok(),
            _ = tokio::time::sleep(Self::FEEDER_STOP_TIMEOUT) => {
                task.abort();
                false
            }
        }
    }

    /// Whether the source's feeder task is still running. A finished task
    /// means its transport faulted (the handle keeps serving last-good);
    /// recovery is session re-attach, which replaces feeders wholesale.
    /// Lock-free read of task state; never touches the worker.
    pub fn feeder_alive(&self, source_id: SourceId) -> bool {
        self.feeders
            .lock()
            .expect("shared feeders poisoned")
            .get(&source_id)
            .is_some_and(|(task, _)| !task.is_finished())
    }

    /// Explicit stop of a worker-owned capture. Its feeder is dropped
    /// first (no more ticks for a dead capture), then the worker stops
    /// it; a final best-effort poll publishes the terminal snapshot into
    /// the retained handle so stop/error diagnostics (notably
    /// `last_error`) stay readable afterwards. A failed final poll never
    /// fails the stop itself.
    ///
    /// Lock discipline (a past self-deadlock): every guard here lives in
    /// its own statement. The client guard from the final poll must drop
    /// before `session_string` locks the client again, and the handles
    /// guard must drop before any await — tokio mutexes are not
    /// reentrant and std guards must never span awaits.
    pub async fn stop_source(&self, source_id: SourceId) -> Result<(), String> {
        // Cooperatively stop the feeder first so the request below runs
        // on a clean client. If the feeder would not exit in bound, the
        // stop still proceeds: a retired transport then surfaces as an
        // honest unknown-outcome error instead of a fake clean stop.
        let feeder_clean = self.stop_feeder(source_id).await;
        if let Err(error) = self.client.lock().await.request_stop(source_id).await {
            if !feeder_clean {
                return Err(format!(
                    "stop outcome unknown (feeder would not exit, transport may be retired): {error}"
                ));
            }
            return Err(error);
        }
        let final_tick = self.client.lock().await.poll_progress(source_id).await.ok();
        let handle = self
            .handles
            .lock()
            .expect("shared handles poisoned")
            .get(&source_id)
            .cloned();
        let session = self.session_string().await;
        if let (Some(handle), Some(tick)) = (handle, final_tick) {
            let _ =
                tokio::task::spawn_blocking(move || handle.update_progress(&session, tick)).await;
        }
        self.handles
            .lock()
            .expect("shared handles poisoned")
            .remove(&source_id);
        Ok(())
    }

    /// Explicit restart of a worker-owned capture. The worker restarts the
    /// remembered definition; this never invents one. The previous feeder
    /// and handle are replaced: adapters re-register the returned handle,
    /// exactly like a local restart hands out a fresh identity.
    pub async fn restart_source(
        &self,
        definition: &SourceDefinition,
    ) -> Result<SharedSource, String> {
        // Restart addresses the remembered definition by id; a definition
        // the worker never saw is refused there, loudly.
        let started = match self
            .client
            .lock()
            .await
            .request_restart(definition.id)
            .await?
        {
            StartOutcome::Started {
                source_id,
                journal_path,
                ..
            } => RemoteSource {
                source_id,
                journal_path,
            },
            StartOutcome::StdinBound { .. } => {
                return Err("shared capture cannot drive a forwarded stdin pipe: restart stdin sources window-locally".into());
            }
        };
        let handle = self.feed_handle(&started).await?;
        Ok(SharedSource {
            remote: started,
            handle,
        })
    }

    /// Bounded shutdown drain: every feeder aborts first (no ticks race
    /// the goodbye), then flush, goodbye, bounded close wait through the
    /// shared client (no single owner can drop it while feeders reference
    /// it), viewer lock released on drop. A flush failure returns before
    /// goodbye (no detach on a failed drain); dropping the store detaches
    /// via EOF either way.
    pub async fn drain_and_detach(self) -> Result<(), String> {
        // Signal every feeder first so no new ticks start; each is then
        // awaited through at most one bounded in-flight exchange. Feeders
        // that will not exit are aborted, and a retired transport then
        // surfaces honestly from the flush below instead of a fake clean
        // drain. Viewer lock releases on drop either way.
        let feeders = std::mem::take(&mut *self.feeders.lock().expect("shared feeders poisoned"));
        for (_, (mut task, stop)) in feeders {
            stop.store(true, std::sync::atomic::Ordering::Release);
            tokio::select! {
                _ = &mut task => {}
                _ = tokio::time::sleep(Self::FEEDER_STOP_TIMEOUT) => {
                    task.abort();
                }
            }
        }
        self.client.lock().await.detach().await
    }

    /// Load persisted views for one source through the worker. The returned
    /// views carry their persisted versions — the values later saves must
    /// echo back — exactly like the local load path.
    pub async fn load_views(
        &self,
        definition: SourceDefinition,
        view_id: ViewId,
    ) -> Result<MemoryEvent, String> {
        let request_id = self.take_request_id();
        let event = self
            .client
            .lock()
            .await
            .store(StoreMethod::Load {
                request_id,
                window_id: String::new(),
                definition,
                view_id,
            })
            .await?;
        match event {
            StoreEvent::Loaded {
                source_id,
                view_id,
                views,
                ..
            } => {
                // Mirror local load: every returned view's persisted version
                // becomes its base, so the next save carries truth instead
                // of None-against-an-existing-row.
                self.bases
                    .lock()
                    .expect("shared bases poisoned")
                    .seed_from_loaded(&views);
                Ok(MemoryEvent::Loaded(source_id, view_id, views))
            }
            StoreEvent::LoadFailed {
                source_id,
                view_id,
                reason,
                ..
            } => Ok(MemoryEvent::LoadFailed(source_id, view_id, reason)),
            unexpected => Err(unexpected_reply("load", &unexpected)),
        }
    }

    /// Persist one view through the worker. The last committed version (or
    /// none for a view never saved this session) travels as the
    /// compare-and-swap base; the reply updates it. The echoed sequence —
    /// not a version — is what the app correlates on, exactly like local.
    /// A conflict keeps the last-success base (see [`SaveBases`]): the
    /// automatic follow-up save is refused again, never overwriting the
    /// peer, until a reconciled reload reseeds.
    pub async fn save_view(&self, request: &SaveRequest) -> Result<MemoryEvent, String> {
        let expected_version = self
            .bases
            .lock()
            .expect("shared bases poisoned")
            .base_for(request.view_id);
        let request_id = self.take_request_id();
        let event = self
            .client
            .lock()
            .await
            .store(StoreMethod::Save {
                request_id,
                window_id: String::new(),
                sequence: request.sequence,
                definition: request.definition.clone(),
                view_id: request.view_id,
                state: crate::memory::working_view(request),
                expected_version,
            })
            .await?;
        match event {
            StoreEvent::Saved {
                source_id,
                view_id,
                sequence,
                version,
                ..
            } => {
                self.bases
                    .lock()
                    .expect("shared bases poisoned")
                    .note_saved(view_id, version);
                Ok(MemoryEvent::Saved(source_id, view_id, sequence))
            }
            StoreEvent::SaveFailed {
                source_id,
                view_id,
                sequence,
                reason,
                ..
            } => {
                self.bases
                    .lock()
                    .expect("shared bases poisoned")
                    .note_failed(view_id);
                Ok(MemoryEvent::SaveFailed(
                    source_id, view_id, sequence, reason,
                ))
            }
            unexpected => Err(unexpected_reply("save", &unexpected)),
        }
    }

    /// Persist a derived view before it is shown, mirroring the local reply
    /// contract: only success may make the view visible. The worker answers
    /// a create with `Saved` carrying the version read back post-commit
    /// (there is no separate create event on the wire); that echoed version
    /// seeds the base, mirroring local create seeding version 0.
    pub async fn create_derived_view(&self, request: &SaveRequest) -> Result<MemoryEvent, String> {
        let request_id = self.take_request_id();
        let event = self
            .client
            .lock()
            .await
            .store(StoreMethod::CreateDerivedView {
                request_id,
                window_id: String::new(),
                sequence: request.sequence,
                definition: request.definition.clone(),
                view_id: request.view_id,
                state: crate::memory::working_view(request),
            })
            .await?;
        match event {
            StoreEvent::Saved {
                view_id, version, ..
            } => {
                self.bases
                    .lock()
                    .expect("shared bases poisoned")
                    .seed_created(view_id, version);
                Ok(MemoryEvent::DerivedViewCreated(view_id, Ok(())))
            }
            StoreEvent::SaveFailed {
                view_id, reason, ..
            } => {
                self.bases
                    .lock()
                    .expect("shared bases poisoned")
                    .note_failed(view_id);
                Ok(MemoryEvent::DerivedViewCreated(view_id, Err(reason)))
            }
            unexpected => Err(unexpected_reply("create-derived-view", &unexpected)),
        }
    }

    /// List recent sources through the worker.
    pub async fn recent_sources(&self) -> Result<MemoryEvent, String> {
        let request_id = self.take_request_id();
        let event = self
            .client
            .lock()
            .await
            .store(StoreMethod::Recent {
                request_id,
                window_id: String::new(),
            })
            .await?;
        match event {
            StoreEvent::Recent { sources, .. } => Ok(MemoryEvent::Recent(sources)),
            StoreEvent::RecentFailed { reason, .. } => Ok(MemoryEvent::RecentFailed(reason)),
            unexpected => Err(unexpected_reply("recent", &unexpected)),
        }
    }

    /// Recipe catalogue lookup through the worker (enrichment flows use the
    /// same mediated path, so assistance capabilities are preserved).
    pub async fn list_recipes(
        &self,
        meta: &RecipeRequestMeta,
        context: &Option<SuggestionContext>,
    ) -> Result<MemoryEvent, String> {
        let request_id = self.take_request_id();
        let event = self
            .client
            .lock()
            .await
            .store(StoreMethod::ListRecipes {
                request_id,
                window_id: String::new(),
                meta: wire_meta(meta),
                context: context.as_ref().map(wire_context),
            })
            .await?;
        match event {
            StoreEvent::Recipes {
                meta,
                recipes,
                candidates,
                ..
            } => Ok(MemoryEvent::Recipes(app_meta(meta), recipes, candidates)),
            StoreEvent::RecipeFailed { meta, reason, .. } => {
                Ok(MemoryEvent::RecipeFailed(app_meta(meta), reason))
            }
            unexpected => Err(unexpected_reply("list-recipes", &unexpected)),
        }
    }

    /// Recipe revision history through the worker.
    pub async fn recipe_history(
        &self,
        meta: &RecipeRequestMeta,
        id: lvu_core::RecipeId,
    ) -> Result<MemoryEvent, String> {
        let request_id = self.take_request_id();
        let event = self
            .client
            .lock()
            .await
            .store(StoreMethod::RecipeHistory {
                request_id,
                window_id: String::new(),
                meta: wire_meta(meta),
                recipe_id: id,
            })
            .await?;
        match event {
            StoreEvent::RecipeHistory {
                meta, revisions, ..
            } => Ok(MemoryEvent::RecipeHistory(app_meta(meta), revisions)),
            StoreEvent::RecipeFailed { meta, reason, .. } => {
                Ok(MemoryEvent::RecipeFailed(app_meta(meta), reason))
            }
            unexpected => Err(unexpected_reply("recipe-history", &unexpected)),
        }
    }

    /// Persist a recipe revision through the worker.
    pub async fn save_recipe(
        &self,
        meta: &RecipeRequestMeta,
        recipe: lvu_memory::RecipeFile,
        expected_revision: Option<uuid::Uuid>,
        context: &Option<SuggestionContext>,
    ) -> Result<MemoryEvent, String> {
        let request_id = self.take_request_id();
        let event = self
            .client
            .lock()
            .await
            .store(StoreMethod::SaveRecipe {
                request_id,
                window_id: String::new(),
                meta: wire_meta(meta),
                recipe,
                expected_revision,
                context: context.as_ref().map(wire_context),
            })
            .await?;
        match event {
            StoreEvent::RecipeSaved { meta, saved, .. } => {
                Ok(MemoryEvent::RecipeSaved(app_meta(meta), saved))
            }
            StoreEvent::RecipeFailed { meta, reason, .. } => {
                Ok(MemoryEvent::RecipeFailed(app_meta(meta), reason))
            }
            unexpected => Err(unexpected_reply("save-recipe", &unexpected)),
        }
    }

    /// Import a recipe file through the worker.
    pub async fn import_recipe(
        &self,
        meta: &RecipeRequestMeta,
        path: PathBuf,
    ) -> Result<MemoryEvent, String> {
        let request_id = self.take_request_id();
        let event = self
            .client
            .lock()
            .await
            .store(StoreMethod::ImportRecipe {
                request_id,
                window_id: String::new(),
                meta: wire_meta(meta),
                path,
            })
            .await?;
        match event {
            StoreEvent::RecipeSaved { meta, saved, .. } => {
                Ok(MemoryEvent::RecipeSaved(app_meta(meta), saved))
            }
            StoreEvent::RecipeFailed { meta, reason, .. } => {
                Ok(MemoryEvent::RecipeFailed(app_meta(meta), reason))
            }
            unexpected => Err(unexpected_reply("import-recipe", &unexpected)),
        }
    }

    /// Export a recipe revision through the worker.
    pub async fn export_recipe(
        &self,
        meta: &RecipeRequestMeta,
        recipe: lvu_core::RecipeId,
        revision: uuid::Uuid,
        path: PathBuf,
    ) -> Result<MemoryEvent, String> {
        let request_id = self.take_request_id();
        let event = self
            .client
            .lock()
            .await
            .store(StoreMethod::ExportRecipe {
                request_id,
                window_id: String::new(),
                meta: wire_meta(meta),
                recipe_id: recipe,
                revision,
                path,
            })
            .await?;
        match event {
            StoreEvent::RecipeExported { meta, saved, .. } => {
                Ok(MemoryEvent::RecipeExported(app_meta(meta), saved))
            }
            StoreEvent::RecipeFailed { meta, reason, .. } => {
                Ok(MemoryEvent::RecipeFailed(app_meta(meta), reason))
            }
            unexpected => Err(unexpected_reply("export-recipe", &unexpected)),
        }
    }

    /// Record a recipe suggestion outcome through the worker. Like the local
    /// path this is fire-and-forget on success: `None` means recorded,
    /// `Some` carries the failure event.
    pub async fn record_suggestion(
        &self,
        outcome: &RecipeOutcome,
    ) -> Result<Option<MemoryEvent>, String> {
        let request_id = self.take_request_id();
        let event = self
            .client
            .lock()
            .await
            .store(StoreMethod::RecordSuggestion {
                request_id,
                window_id: String::new(),
                outcome: wire_outcome(outcome),
            })
            .await?;
        match event {
            StoreEvent::SuggestionRecorded { .. } => Ok(None),
            StoreEvent::SuggestionFailed { reason, .. } => {
                Ok(Some(MemoryEvent::SuggestionFailed(reason)))
            }
            unexpected => Err(unexpected_reply("record-suggestion", &unexpected)),
        }
    }
}

fn wire_meta(meta: &RecipeRequestMeta) -> lvu_shared::RequestMeta {
    lvu_shared::RequestMeta {
        request_id: meta.request_id,
        dialog_id: meta.dialog_id,
        dialog_revision: meta.dialog_revision,
    }
}

fn app_meta(meta: lvu_shared::RequestMeta) -> RecipeRequestMeta {
    RecipeRequestMeta {
        request_id: meta.request_id,
        dialog_id: meta.dialog_id,
        dialog_revision: meta.dialog_revision,
    }
}

fn wire_context(context: &SuggestionContext) -> SuggestionContextShape {
    SuggestionContextShape {
        source: context.source,
        project: context.project.clone(),
        command: context.command.clone(),
        fields: context.fields.clone(),
    }
}

fn wire_outcome(outcome: &RecipeOutcome) -> SuggestionOutcomeShape {
    SuggestionOutcomeShape {
        source_id: outcome.source_id.clone(),
        recipe_id: outcome.recipe_id.clone(),
        revision: outcome.revision.clone(),
        accepted: outcome.accepted,
    }
}

/// A reply that is not the answer to this request: loud and bounded (no
/// payload interpolation — a `Loaded` batch must never land in an error
/// string), never guessed into shape.
fn unexpected_reply(method: &str, event: &StoreEvent) -> String {
    let _ = event;
    format!("shared store answered {method} with an unexpected reply shape")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    use lvu_shared::{AdmissionHook, AdmissionVerdict, WorkerConfig, WorkerService};

    struct AdmitAll;

    impl AdmissionHook for AdmitAll {
        fn admit(&self, _definition: &SourceDefinition) -> AdmissionVerdict {
            AdmissionVerdict::Admit
        }
    }

    fn test_definition(id: u128, path: &Path) -> SourceDefinition {
        SourceDefinition {
            schema_version: 1,
            id: SourceId(uuid::Uuid::from_u128(id)),
            name: format!("log-{id}"),
            acquisition: lvu_core::Acquisition::File {
                path: path.to_path_buf(),
                follow: true,
            },
            identity_hints: Default::default(),
            retention: None,
        }
    }

    fn test_request(sequence: u64, definition: &SourceDefinition, view: ViewId) -> SaveRequest {
        SaveRequest {
            sequence,
            definition: definition.clone(),
            view_id: view,
            state: lvu::PersistentViewState::default(),
        }
    }

    /// One serving worker plus its socket: windows attach with distinct
    /// viewer pids (the election refuses two takes of one slot).
    struct Fixture {
        capture_root: PathBuf,
        socket: PathBuf,
    }

    async fn serving(root: &Path) -> Fixture {
        let capture_root = root.join("captures");
        let paths = lvu_shared::WorkerPaths::new(&capture_root);
        paths.ensure_directories().unwrap();
        let config = WorkerConfig {
            capture_root: capture_root.clone(),
            workspace_root: capture_root.join("workspace"),
            socket_path: paths.socket_path(),
            viewer_grace: Duration::from_millis(100),
            request_timeout: Duration::from_secs(10),
        };
        let (service, _) = WorkerService::open(config, Arc::new(AdmitAll)).unwrap();
        let listener = tokio::net::UnixListener::bind(paths.socket_path()).unwrap();
        tokio::spawn(async move {
            service.serve(listener).await;
        });
        Fixture {
            capture_root,
            socket: paths.socket_path(),
        }
    }

    async fn attach_window(fixture: &Fixture, window: &str, pid: u32) -> SharedStore {
        let (client, _) =
            WorkerClient::connect(&fixture.capture_root, &fixture.socket, window, pid)
                .await
                .expect("window attaches");
        SharedStore::from_client(client)
    }

    #[tokio::test]
    async fn load_seeds_base_then_save_commits() {
        let root = tempfile::tempdir().unwrap();
        let log = root.path().join("v.log");
        std::fs::write(&log, "one\n").unwrap();
        let fixture = serving(root.path()).await;
        let window = attach_window(&fixture, "window-a", 6101).await;
        let definition = test_definition(21, &log);
        let view = ViewId(uuid::Uuid::from_u128(22));
        // Fresh save inserts at version 0 with no base.
        match window
            .save_view(&test_request(1, &definition, view))
            .await
            .expect("save answers")
        {
            MemoryEvent::Saved(_, _, sequence) => assert_eq!(sequence, 1),
            other => panic!("expected saved, got {other:?}"),
        }
        // Reload reseeds the committed version as the base...
        let views = match window
            .load_views(definition.clone(), view)
            .await
            .expect("load answers")
        {
            MemoryEvent::Loaded(_, _, views) => views,
            other => panic!("expected loaded, got {other:?}"),
        };
        assert!(views.iter().any(|loaded| loaded.id == view));
        // ...so the next save carries truth and commits.
        match window
            .save_view(&test_request(2, &definition, view))
            .await
            .expect("save answers")
        {
            MemoryEvent::Saved(_, _, sequence) => assert_eq!(sequence, 2),
            other => panic!("expected saved, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_then_save_commits() {
        let root = tempfile::tempdir().unwrap();
        let log = root.path().join("v.log");
        std::fs::write(&log, "one\n").unwrap();
        let fixture = serving(root.path()).await;
        let window = attach_window(&fixture, "window-a", 6102).await;
        let definition = test_definition(23, &log);
        let view = ViewId(uuid::Uuid::from_u128(24));
        match window
            .create_derived_view(&test_request(1, &definition, view))
            .await
            .expect("create answers")
        {
            MemoryEvent::DerivedViewCreated(created, Ok(())) => assert_eq!(created, view),
            other => panic!("expected created, got {other:?}"),
        }
        // The echoed version seeded the base: this save commits, it does
        // not conflict-forever on a missing base.
        match window
            .save_view(&test_request(2, &definition, view))
            .await
            .expect("save answers")
        {
            MemoryEvent::Saved(_, _, sequence) => assert_eq!(sequence, 2),
            other => panic!("expected saved, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn conflict_then_automatic_change_still_refused() {
        let root = tempfile::tempdir().unwrap();
        let log = root.path().join("v.log");
        std::fs::write(&log, "one\n").unwrap();
        let fixture = serving(root.path()).await;
        let first = attach_window(&fixture, "window-a", 6103).await;
        let second = attach_window(&fixture, "window-b", 6104).await;
        let definition = test_definition(25, &log);
        let view = ViewId(uuid::Uuid::from_u128(26));
        // Window A commits version 0; window B loads (seeding its base)
        // and wins version 1 against it.
        match first
            .save_view(&test_request(1, &definition, view))
            .await
            .expect("save answers")
        {
            MemoryEvent::Saved(..) => {}
            other => panic!("expected saved, got {other:?}"),
        }
        match second
            .load_views(definition.clone(), view)
            .await
            .expect("load answers")
        {
            MemoryEvent::Loaded(..) => {}
            other => panic!("expected loaded, got {other:?}"),
        }
        match second
            .save_view(&test_request(1, &definition, view))
            .await
            .expect("save answers")
        {
            MemoryEvent::Saved(..) => {}
            other => panic!("expected saved, got {other:?}"),
        }
        // Window A's stale save loses with the conflict surfaced...
        match first
            .save_view(&test_request(2, &definition, view))
            .await
            .expect("save answers")
        {
            MemoryEvent::SaveFailed(..) => {}
            other => panic!("expected conflict, got {other:?}"),
        }
        // ...and the automatic follow-up (same stale draft, new sequence,
        // e.g. a bookmark tick) is refused AGAIN — the base never moved,
        // so the peer is never overwritten.
        match first
            .save_view(&test_request(3, &definition, view))
            .await
            .expect("save answers")
        {
            MemoryEvent::SaveFailed(..) => {}
            other => panic!("expected repeated conflict, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn conflict_then_reload_recovers() {
        let root = tempfile::tempdir().unwrap();
        let log = root.path().join("v.log");
        std::fs::write(&log, "one\n").unwrap();
        let fixture = serving(root.path()).await;
        let first = attach_window(&fixture, "window-a", 6105).await;
        let second = attach_window(&fixture, "window-b", 6106).await;
        let definition = test_definition(27, &log);
        let view = ViewId(uuid::Uuid::from_u128(28));
        // Window A commits version 0; window B loads first (a baseless
        // first save against A's row would conflict by design) and wins
        // version 1.
        match first
            .save_view(&test_request(1, &definition, view))
            .await
            .expect("save answers")
        {
            MemoryEvent::Saved(..) => {}
            other => panic!("expected saved, got {other:?}"),
        }
        match second
            .load_views(definition.clone(), view)
            .await
            .expect("load answers")
        {
            MemoryEvent::Loaded(..) => {}
            other => panic!("expected loaded, got {other:?}"),
        }
        match second
            .save_view(&test_request(1, &definition, view))
            .await
            .expect("save answers")
        {
            MemoryEvent::Saved(..) => {}
            other => panic!("expected saved, got {other:?}"),
        }
        // Window A loses against window B's version 1...
        match first
            .save_view(&test_request(2, &definition, view))
            .await
            .expect("save answers")
        {
            MemoryEvent::SaveFailed(..) => {}
            other => panic!("expected conflict, got {other:?}"),
        }
        // ...reloads (reseeding the base to truth), merges, and converges.
        match first
            .load_views(definition.clone(), view)
            .await
            .expect("load answers")
        {
            MemoryEvent::Loaded(..) => {}
            other => panic!("expected loaded, got {other:?}"),
        }
        match first
            .save_view(&test_request(3, &definition, view))
            .await
            .expect("save answers")
        {
            MemoryEvent::Saved(..) => {}
            other => panic!("expected recovery save, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn start_feeds_progress_and_stop_publishes_terminal() {
        let root = tempfile::tempdir().unwrap();
        let log = root.path().join("v.log");
        std::fs::write(&log, "one\n").unwrap();
        let fixture = serving(root.path()).await;
        let store = attach_window(&fixture, "window-a", 6107).await;
        let definition = test_definition(29, &log);
        // Acquisition through the worker returns adapter-ready input plus
        // a live read handle with a running feeder.
        let shared = store.start_source(&definition).await.expect("start");
        assert_eq!(shared.remote.source_id, definition.id);
        assert!(shared.remote.journal_path.ends_with("capture.journal"));
        // The feeder keeps the cache fresh without any caller polling:
        // append rows, then wait (bounded) for the tick to land them.
        std::fs::write(&log, "one\ntwo\nthree\n").unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        loop {
            if shared.handle.progress().records >= 3 {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!("feeder never published appended rows");
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        // Stop drops the feeder and publishes the terminal snapshot, so
        // diagnostics stay readable after the capture ends.
        store.stop_source(definition.id).await.expect("stop");
        assert!(shared.handle.progress().state.is_terminal());
    }

    /// Scripted peer with a request-kind log. The decision function maps
    /// each request kind to an optional reply (`None` = stay silent, for
    /// pending polls); every kind seen is recorded so tests assert what
    /// reached the worker.
    struct ScriptedPeer {
        seen: Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl ScriptedPeer {
        fn new() -> (Self, Arc<std::sync::Mutex<Vec<String>>>) {
            let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
            (
                Self {
                    seen: Arc::clone(&seen),
                },
                seen,
            )
        }

        async fn serve(
            self,
            listener: tokio::net::UnixListener,
            respond: impl Fn(&str, &str) -> Option<lvu_shared::WorkerEvent> + Send + 'static,
        ) {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let (stream, _) = listener.accept().await.expect("peer accepts");
            let (reader, mut writer) = stream.into_split();
            let mut reader = tokio::io::BufReader::new(reader);
            let mut decoder = lvu_shared::FrameDecoder::new();
            let mut buffer = vec![0u8; lvu_shared::READ_CHUNK_BYTES];
            loop {
                let count = reader.read(&mut buffer).await.expect("peer reads");
                if count == 0 {
                    return;
                }
                let values = decoder.push_bytes(&buffer[..count]).expect("peer decodes");
                for value in values {
                    let kind = value
                        .get("method")
                        .and_then(|kind| kind.as_str())
                        .unwrap_or("")
                        .to_owned();
                    let id = value
                        .get("request_id")
                        .and_then(|id| id.as_str())
                        .unwrap_or("")
                        .to_owned();
                    self.seen.lock().expect("seen poisoned").push(kind.clone());
                    if kind == "goodbye" {
                        return;
                    }
                    if let Some(reply) = respond(&kind, &id) {
                        let wire = lvu_shared::encode_frame(
                            &serde_json::to_value(&reply).expect("peer encodes"),
                        )
                        .expect("peer frames");
                        writer.write_all(&wire).await.expect("peer writes");
                    }
                }
            }
        }
    }

    fn test_progress(
        source_id: SourceId,
        generation: u64,
        terminal: bool,
    ) -> lvu_ingest::SourceProgress {
        lvu_ingest::SourceProgress {
            source_id,
            generation,
            state: if terminal {
                lvu_ingest::RuntimeState::Stopped
            } else {
                lvu_ingest::RuntimeState::Running
            },
            records: 1,
            high_watermark: None,
            journal_bytes: 0,
            synced_records: 0,
            syncs: 0,
            handovers: 0,
            writer_cpu_nanos: 0,
            reader_cpu_nanos: 0,
            boundaries: 0,
            exit_code: None,
            discarded_bytes: 0,
            discarded_bytes_known: true,
            last_error: None,
        }
    }
    /// Stop during a pending feeder poll against a silent peer: the stop
    /// waits out the bounded in-flight exchange cooperatively, but the
    /// transport fault retires the client — so the stop itself must NOT
    /// be sent on the poisoned stream. It reports unknown-outcome instead:
    /// the capture may or may not have stopped, and only re-attach can
    /// reconcile. The peer observably never receives the stop.
    #[tokio::test]
    async fn stop_during_pending_poll_reports_unknown() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let root = tempfile::tempdir().unwrap();
        lvu_shared::WorkerPaths::new(root.path())
            .ensure_directories()
            .expect("viewer directories");
        let socket = root.path().join("control.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let source_id = SourceId(uuid::Uuid::from_u128(901));
        let journal = root.path().join("capture.journal");
        std::fs::write(&journal, "").unwrap();
        let polls = Arc::new(AtomicU32::new(0));
        let (peer, seen) = ScriptedPeer::new();
        let respond_polls = Arc::clone(&polls);
        let respond_source = source_id;
        let respond_journal = journal.clone();
        tokio::spawn(peer.serve(listener, move |kind, id| {
            let n = match kind {
                "request_progress" => respond_polls.fetch_add(1, Ordering::SeqCst),
                _ => 0,
            };
            match kind {
                "hello" => Some(lvu_shared::WorkerEvent::Welcome {
                    request_id: id.into(),
                    worker_pid: 1,
                    protocol: lvu_shared::PROTOCOL_VERSION,
                    worker_session: "session-stop".into(),
                    sources: Vec::new(),
                }),
                "request_start" => Some(lvu_shared::WorkerEvent::Started {
                    request_id: id.into(),
                    source_id: respond_source.0.to_string(),
                    journal_path: respond_journal.display().to_string(),
                }),
                "request_progress" if n == 0 => Some(lvu_shared::WorkerEvent::SourceProgress {
                    request_id: id.into(),
                    worker_session: "session-stop".into(),
                    progress: test_progress(respond_source, 1, false),
                }),
                // Second poll (the feeder's first tick) hangs: the peer
                // stays silent so the tick is provably pending at stop.
                "request_progress" if n == 1 => None,
                "request_progress" => Some(lvu_shared::WorkerEvent::SourceProgress {
                    request_id: id.into(),
                    worker_session: "session-stop".into(),
                    progress: test_progress(respond_source, 1, true),
                }),
                "request_stop" => Some(lvu_shared::WorkerEvent::Stopped {
                    request_id: id.into(),
                    source_id: respond_source.0.to_string(),
                }),
                _ => None,
            }
        }));
        let (client, _) = WorkerClient::connect(root.path(), &socket, "window-s", 6207)
            .await
            .expect("connect");
        let store = SharedStore::from_client(client);
        let definition = test_definition(91, &root.path().join("v.log"));
        let shared = store.start_source(&definition).await.expect("start");
        assert_eq!(shared.remote.source_id, source_id);
        assert!(store.feeder_alive(source_id));
        // Let the feeder block in its first tick poll before stopping.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        // The silent peer lets the tick time out and retire the client:
        // the stop must NOT go out on the poisoned stream. It reports
        // unknown-outcome instead of pretending a clean stop.
        let error = store
            .stop_source(source_id)
            .await
            .expect_err("stop on retired transport must report unknown");
        assert!(
            error.contains("unknown") || error.contains("retired"),
            "stop must name the unknown outcome: {error}"
        );
        assert!(!store.feeder_alive(source_id));
        let seen = seen.lock().expect("seen poisoned");
        assert!(
            !seen.iter().any(|kind| kind == "request_stop"),
            "nothing must be sent on the retired stream, saw: {seen:?}"
        );
    }

    /// Stop against a responsive peer: the feeder idles cooperatively,
    /// the stop reaches the worker, and the terminal snapshot publishes
    /// into the retained handle.
    #[tokio::test]
    async fn stop_reaches_live_worker_and_publishes_terminal() {
        let root = tempfile::tempdir().unwrap();
        lvu_shared::WorkerPaths::new(root.path())
            .ensure_directories()
            .expect("viewer directories");
        let socket = root.path().join("control.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let source_id = SourceId(uuid::Uuid::from_u128(903));
        let journal = root.path().join("capture.journal");
        std::fs::write(&journal, "").unwrap();
        let (peer, seen) = ScriptedPeer::new();
        let respond_source = source_id;
        let respond_journal = journal.clone();
        // The peer models liveness: once stopped, polls answer terminal,
        // like a real worker retaining the stopped handle.
        let stopped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let respond_stopped = std::sync::Arc::clone(&stopped);
        tokio::spawn(peer.serve(listener, move |kind, id| match kind {
            "hello" => Some(lvu_shared::WorkerEvent::Welcome {
                request_id: id.into(),
                worker_pid: 1,
                protocol: lvu_shared::PROTOCOL_VERSION,
                worker_session: "session-stop-ok".into(),
                sources: Vec::new(),
            }),
            "request_start" => Some(lvu_shared::WorkerEvent::Started {
                request_id: id.into(),
                source_id: respond_source.0.to_string(),
                journal_path: respond_journal.display().to_string(),
            }),
            "request_progress" => {
                let terminal = respond_stopped.load(std::sync::atomic::Ordering::Acquire);
                Some(lvu_shared::WorkerEvent::SourceProgress {
                    request_id: id.into(),
                    worker_session: "session-stop-ok".into(),
                    progress: test_progress(respond_source, 1, terminal),
                })
            }
            "request_stop" => {
                respond_stopped.store(true, std::sync::atomic::Ordering::Release);
                Some(lvu_shared::WorkerEvent::Stopped {
                    request_id: id.into(),
                    source_id: respond_source.0.to_string(),
                })
            }
            _ => None,
        }));
        let (client, _) = WorkerClient::connect(root.path(), &socket, "window-s", 6209)
            .await
            .expect("connect");
        let store = SharedStore::from_client(client);
        let definition = test_definition(93, &root.path().join("v.log"));
        let shared = store.start_source(&definition).await.expect("start");
        assert!(store.feeder_alive(source_id));
        // Let a tick or two flow so the feeder is genuinely idling (not
        // still starting) when stop arrives.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        store
            .stop_source(source_id)
            .await
            .expect("stop reaches worker");
        assert!(!store.feeder_alive(source_id));
        let seen = seen.lock().expect("seen poisoned");
        assert!(
            seen.iter().any(|kind| kind == "request_stop"),
            "stop must reach the worker, saw: {seen:?}"
        );
        // Terminal snapshot published into the retained handle.
        assert!(shared.handle.progress().state.is_terminal());
    }

    /// Drain with a live feeder: the drain signals feeders cooperatively,
    /// flushes, says goodbye, and the peer observes the full sequence.
    /// No abort ever races the goodbye.
    #[tokio::test]
    async fn drain_detaches_after_cooperative_feeder_stop() {
        let root = tempfile::tempdir().unwrap();
        lvu_shared::WorkerPaths::new(root.path())
            .ensure_directories()
            .expect("viewer directories");
        let socket = root.path().join("control.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let source_id = SourceId(uuid::Uuid::from_u128(902));
        let journal = root.path().join("capture.journal");
        std::fs::write(&journal, "").unwrap();
        let (peer, seen) = ScriptedPeer::new();
        let respond_source = source_id;
        let respond_journal = journal.clone();
        tokio::spawn(peer.serve(listener, move |kind, id| match kind {
            "hello" => Some(lvu_shared::WorkerEvent::Welcome {
                request_id: id.into(),
                worker_pid: 1,
                protocol: lvu_shared::PROTOCOL_VERSION,
                worker_session: "session-drain".into(),
                sources: Vec::new(),
            }),
            "request_start" => Some(lvu_shared::WorkerEvent::Started {
                request_id: id.into(),
                source_id: respond_source.0.to_string(),
                journal_path: respond_journal.display().to_string(),
            }),
            "request_progress" => Some(lvu_shared::WorkerEvent::SourceProgress {
                request_id: id.into(),
                worker_session: "session-drain".into(),
                progress: test_progress(respond_source, 1, false),
            }),
            "flush" => Some(lvu_shared::WorkerEvent::Store(
                lvu_shared::StoreEvent::Flushed {
                    request_id: id.into(),
                },
            )),
            _ => None,
        }));
        let (client, _) = WorkerClient::connect(root.path(), &socket, "window-s", 6208)
            .await
            .expect("connect");
        let store = SharedStore::from_client(client);
        let definition = test_definition(92, &root.path().join("v.log"));
        let _shared = store.start_source(&definition).await.expect("start");
        assert!(store.feeder_alive(source_id));
        // Drain consumes the session: no post-drain handle access is
        // possible by construction. Detach is proven by the peer-visible
        // flush + goodbye below, and abort-safety by the stop test above
        // (no abort races the goodbye there either).
        store.drain_and_detach().await.expect("drain");
        let seen = seen.lock().expect("seen poisoned");
        assert!(
            seen.iter().any(|kind| kind == "flush"),
            "drain must flush, saw: {seen:?}"
        );
        assert!(
            seen.iter().any(|kind| kind == "goodbye"),
            "drain must say goodbye, saw: {seen:?}"
        );
    }
}
