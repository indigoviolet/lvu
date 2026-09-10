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
//! workspace sqlite has exactly one writer (the worker). Every store call
//! below — views, recipes, suggestions — goes through the worker; there is
//! no local fallback that could split-brain durable state.
//!
//! Transport-complete but session-unconsumed until the handle seam lands:
//! `startup` drives the lifecycle, while the per-source and per-save entry
//! points below wait for the `StartedSource`/controller cutover. The allow
//! lifts with that cutover; until then an uncalled function here is pending
//! wiring, never dead design.
#![allow(dead_code)]
//!
//! Version tracking mirrors the local worker thread's `versions` map: the
//! last committed version per view travels as the next save's base, and a
//! conflict teaches the winner's version (see `note_saved` / `note_failed`).
//! One deliberate difference from local: local never learns on failure, but
//! there a conflict means a foreign process touched the database behind its
//! back; here the winner is a live peer whose version is truth, and holding
//! a stale base would brick the view's persistence with no recovery path.
//! The UX contract is unchanged either way: the failure surfaces as a
//! conflict and only an explicit user rebase retries.
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

use lvu::{RecipeRequestMeta, app::RecipeOutcome};
use lvu_core::{SourceDefinition, SourceId, ViewId};
use lvu_shared::{
    StartOutcome, StoreEvent, StoreMethod, SuggestionContextShape, SuggestionOutcomeShape,
    WorkerClient,
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

/// One window's shared session: the worker connection plus the
/// last-committed version per view (the compare-and-swap base, mirroring
/// the local worker thread's `versions` map).
pub struct SharedStore {
    client: WorkerClient,
    versions: HashMap<ViewId, u64>,
    next_request: u64,
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
        Ok(Self {
            client,
            versions: HashMap::new(),
            next_request: 1,
        })
    }

    fn take_request_id(&mut self) -> String {
        let id = format!("shared-{}", self.next_request);
        self.next_request += 1;
        id
    }

    /// Record a commit: the next save for this view carries it as base.
    fn note_saved(&mut self, view_id: ViewId, version: u64) {
        self.versions.insert(view_id, version);
    }

    /// Record a conflict loss: the winner's version becomes the base, so a
    /// later save after an explicit user rebase can converge instead of
    /// conflicting forever. The failure itself still surfaces to the
    /// caller; learning the base never acks unwritten state.
    fn note_failed(&mut self, view_id: ViewId, current_version: Option<u64>) {
        if let Some(version) = current_version {
            self.versions.insert(view_id, version);
        }
    }

    /// Explicit user-approved acquisition through the worker. The definition
    /// travels as its canonical DTO; admission (identity dedup) is enforced
    /// worker-side and the live capture is re-presented, never double-started.
    pub async fn start_source(
        &mut self,
        definition: &SourceDefinition,
    ) -> Result<RemoteSource, String> {
        match self.client.request_start(definition).await? {
            StartOutcome::Started {
                source_id,
                journal_path,
                ..
            } => Ok(RemoteSource {
                source_id,
                journal_path,
            }),
            StartOutcome::StdinBound { .. } => Err(
                "shared capture cannot drive a forwarded stdin pipe: start stdin sources window-locally"
                    .into(),
            ),
        }
    }

    /// Explicit stop of a worker-owned capture.
    pub async fn stop_source(&mut self, source_id: SourceId) -> Result<(), String> {
        self.client.request_stop(source_id).await
    }

    /// Explicit restart of a worker-owned capture. The worker restarts the
    /// remembered definition; this never invents one.
    pub async fn restart_source(
        &mut self,
        definition: &SourceDefinition,
    ) -> Result<RemoteSource, String> {
        // Restart addresses the remembered definition by id; a definition
        // the worker never saw is refused there, loudly.
        match self.client.request_restart(definition.id).await? {
            StartOutcome::Started {
                source_id,
                journal_path,
                ..
            } => Ok(RemoteSource {
                source_id,
                journal_path,
            }),
            StartOutcome::StdinBound { .. } => Err(
                "shared capture cannot drive a forwarded stdin pipe: restart stdin sources window-locally"
                    .into(),
            ),
        }
    }

    /// Bounded shutdown drain: flush, goodbye, bounded close wait, viewer
    /// lock released. A flush failure returns before goodbye (no detach on
    /// a failed drain); dropping the store detaches via EOF either way.
    pub async fn drain_and_detach(self) -> Result<(), String> {
        self.client.shutdown().await
    }

    /// Load persisted views for one source through the worker. The returned
    /// views carry their persisted versions — the values later saves must
    /// echo back — exactly like the local load path.
    pub async fn load_views(
        &mut self,
        definition: SourceDefinition,
        view_id: ViewId,
    ) -> Result<MemoryEvent, String> {
        let event = self
            .client
            .store(StoreMethod::Load {
                request_id: self.take_request_id(),
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
            } => Ok(MemoryEvent::Loaded(source_id, view_id, views)),
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
    pub async fn save_view(&mut self, request: &SaveRequest) -> Result<MemoryEvent, String> {
        let expected_version = self.versions.get(&request.view_id).copied();
        let event = self
            .client
            .store(StoreMethod::Save {
                request_id: self.take_request_id(),
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
            } => {
                self.note_saved(view_id, version);
                Ok(MemoryEvent::Saved(source_id, view_id, sequence))
            }
            StoreEvent::SaveFailed {
                source_id,
                view_id,
                sequence,
                reason,
                current_version,
            } => {
                self.note_failed(view_id, current_version);
                Ok(MemoryEvent::SaveFailed(
                    source_id, view_id, sequence, reason,
                ))
            }
            unexpected => Err(unexpected_reply("save", &unexpected)),
        }
    }

    /// Persist a derived view before it is shown, mirroring the local reply
    /// contract: only success may make the view visible.
    pub async fn create_derived_view(
        &mut self,
        request: &SaveRequest,
    ) -> Result<MemoryEvent, String> {
        let event = self
            .client
            .store(StoreMethod::CreateDerivedView {
                request_id: self.take_request_id(),
                window_id: String::new(),
                sequence: request.sequence,
                definition: request.definition.clone(),
                view_id: request.view_id,
                state: crate::memory::working_view(request),
            })
            .await?;
        match event {
            StoreEvent::DerivedViewCreated { view_id, error, .. } => Ok(
                MemoryEvent::DerivedViewCreated(view_id, error.map_or(Ok(()), Err)),
            ),
            unexpected => Err(unexpected_reply("create-derived-view", &unexpected)),
        }
    }

    /// List recent sources through the worker.
    pub async fn recent_sources(&mut self) -> Result<MemoryEvent, String> {
        let event = self
            .client
            .store(StoreMethod::Recent {
                request_id: self.take_request_id(),
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
        &mut self,
        meta: &RecipeRequestMeta,
        context: &Option<SuggestionContext>,
    ) -> Result<MemoryEvent, String> {
        let event = self
            .client
            .store(StoreMethod::ListRecipes {
                request_id: self.take_request_id(),
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
        &mut self,
        meta: &RecipeRequestMeta,
        id: lvu_core::RecipeId,
    ) -> Result<MemoryEvent, String> {
        let event = self
            .client
            .store(StoreMethod::RecipeHistory {
                request_id: self.take_request_id(),
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
        &mut self,
        meta: &RecipeRequestMeta,
        recipe: lvu_memory::RecipeFile,
        expected_revision: Option<uuid::Uuid>,
        context: &Option<SuggestionContext>,
    ) -> Result<MemoryEvent, String> {
        let event = self
            .client
            .store(StoreMethod::SaveRecipe {
                request_id: self.take_request_id(),
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
        &mut self,
        meta: &RecipeRequestMeta,
        path: PathBuf,
    ) -> Result<MemoryEvent, String> {
        let event = self
            .client
            .store(StoreMethod::ImportRecipe {
                request_id: self.take_request_id(),
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
        &mut self,
        meta: &RecipeRequestMeta,
        recipe: lvu_core::RecipeId,
        revision: uuid::Uuid,
        path: PathBuf,
    ) -> Result<MemoryEvent, String> {
        let event = self
            .client
            .store(StoreMethod::ExportRecipe {
                request_id: self.take_request_id(),
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
        &mut self,
        outcome: &RecipeOutcome,
    ) -> Result<Option<MemoryEvent>, String> {
        let event = self
            .client
            .store(StoreMethod::RecordSuggestion {
                request_id: self.take_request_id(),
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
