//! Application controller for live union views.
//!
//! Ownership: Muse union-views assignment. This file is new and owned here;
//! composition-tick wiring in `main.rs` and persistence in `memory.rs` are
//! owned by the primary (autosave) and source-assistance owners respectively,
//! and are proposed verbatim at the bottom of this module (`PROPOSED HOOKS`).
//! Until then it is exercised through
//! `crates/lvu-app/tests/union_controller.rs`, which includes it by path, so
//! no existing controller, worker or manifest is touched.
//!
//! The controller sits between the union dialog and the query runtime and
//! owns exactly three things:
//!
//! * the pending candidate (one per union view ID) with the input revisions
//!   it fenced on at submit time;
//! * the accepted union per union view ID (input IDs plus the revisions they
//!   published under);
//! * the refresh signal: which union views need a re-merge because an input
//!   advanced, vanished, or restarted.
//!
//! It never captures bytes, never assigns record IDs, and never launches
//! commands: restoring a persisted union re-resolves its input view IDs and
//! waits for their accepted revisions like any other view restore. A failed
//! candidate preserves the prior accepted union; staleness is a refresh, not
//! a failure. Persistence shape (`StoredUnion`) is additive serde with
//! defaults throughout, following the `color_rules` precedent — no
//! `DB_SCHEMA_VERSION` bump, older binaries ignore the key.
//!
//! PROPOSED HOOKS (owned elsewhere; do not apply without assignment):
//!
//! 1. `crates/lvu-app/src/main.rs` composition tick (coordinate with the
//!    source-assistance owner, worker 5b39ecc4, for source methods/agent and
//!    UI regions):
//!      - construct `UnionController::default()` beside the command
//!        controller;
//!      - on union dialog acceptance, call `propose(...)`; on query
//!        completion for the union view, call `note_union_published(...)` on
//!        success or `reject(...)` on failure (failure keeps serving the prior
//!        union — the existing last-good rollback, not a new path);
//!      - each tick, drain `take_refreshes()` into union re-merge submissions
//!        through the same fenced `submit` path ordinary views use.
//! 2. `crates/lvu-app/src/memory.rs` + `lvu-memory` presentation (autosave
//!    owner): persist `StoredUnion` under a `presentation_json.union` key with
//!    `#[serde(default)]` on read; restore re-resolves input IDs without
//!    launching remembered commands (the existing restore path already waits
//!    for explicitly opened sources).
//! 3. Archive/export review (primary): union snapshots freeze input view IDs,
//!    their accepted revisions and per-source high-watermarks; independent
//!    input advances create new immutable boundaries rather than mutating a
//!    published manifest.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// How many accepted union views one controller tracks.
///
/// Bounded like every other registry: unions are views, and views already
/// have their own admission cap. This only bounds the controller's map.
pub const MAX_TRACKED_UNIONS: usize = 128;

/// One persisted union input: the input view ID plus the accepted revision
/// the union published under. Shape-compatible with
/// `lvu_view::union::StoredUnionInput` by construction (same JSON); the two
/// must stay in lockstep and the lockstep is asserted in tests.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct StoredUnion {
    #[serde(default)]
    pub inputs: Vec<StoredUnionInput>,
}

/// One persisted union input reference.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct StoredUnionInput {
    #[serde(default)]
    pub view_id: String,
    #[serde(default)]
    pub accepted_revision: u64,
}

/// A submitted-but-unpublished union candidate with its fence baseline.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnionCandidate {
    pub union_view_id: String,
    pub union_revision: u64,
    pub generation: u64,
    pub inputs: Vec<StoredUnionInput>,
}

/// The accepted union a union view is serving.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnionAcceptedState {
    pub inputs: Vec<StoredUnionInput>,
    pub revision: u64,
}

/// A union view that needs a re-merge, and the input revision that moved.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnionRefresh {
    pub union_view_id: String,
    pub union_revision: u64,
    pub moved_view_id: String,
}

/// Every way a union candidate can fail at the controller layer.
///
/// Structural failures (count/duplicates/self/cycle) are reported by the
/// dialog and `lvu-view` layers with their own diagnostics; the controller
/// only fences revisions and capacity. All failures preserve prior accepted
/// state.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum UnionControllerError {
    #[error("unknown union view '{view_id}'")]
    UnknownUnion { view_id: String },
    #[error("no union candidate is pending for '{view_id}'")]
    NoPendingCandidate { view_id: String },
    #[error(
        "union candidate for '{view_id}' is stale: input '{input_id}' moved from {fenced} to {current}"
    )]
    StaleCandidate {
        view_id: String,
        input_id: String,
        fenced: u64,
        current: u64,
    },
    #[error("union input '{input_id}' for '{view_id}' is unavailable")]
    MissingInput { view_id: String, input_id: String },
    #[error("union registry is full ({maximum} tracked unions)")]
    RegistryFull { maximum: usize },
}

/// Application-side union registry. Pure state machine: the composition tick
/// drives it, the query runtime executes under it.
#[derive(Clone, Debug, Default)]
pub struct UnionController {
    pending: HashMap<String, UnionCandidate>,
    accepted: HashMap<String, UnionAcceptedState>,
}

impl UnionController {
    /// Submit a candidate fenced on the given input revisions.
    ///
    /// Overwrites any earlier pending candidate for the same union view: a
    /// newer draft supersedes, never queues behind, the older one.
    pub fn propose(&mut self, candidate: UnionCandidate) {
        self.pending
            .insert(candidate.union_view_id.clone(), candidate);
    }

    /// The pending candidate for a union view, if any.
    pub fn pending(&self, union_view_id: &str) -> Option<&UnionCandidate> {
        self.pending.get(union_view_id)
    }

    /// The accepted union a union view is serving, if any.
    pub fn accepted(&self, union_view_id: &str) -> Option<&UnionAcceptedState> {
        self.accepted.get(union_view_id)
    }

    /// Publish a candidate after the runtime reports success, fencing every
    /// input against its CURRENT accepted revision.
    ///
    /// Any input that moved (or vanished) since submission rejects the
    /// candidate as stale and preserves the prior accepted union: the caller
    /// should rebase the draft onto the new revisions and propose again,
    /// exactly like an ordinary view's base-snapshot mismatch.
    pub fn note_union_published(
        &mut self,
        union_view_id: &str,
        current_revision: impl Fn(&str) -> Option<u64>,
    ) -> Result<UnionAcceptedState, UnionControllerError> {
        let candidate = self.pending.get(union_view_id).ok_or_else(|| {
            UnionControllerError::NoPendingCandidate {
                view_id: union_view_id.to_owned(),
            }
        })?;
        for input in &candidate.inputs {
            match current_revision(&input.view_id) {
                Some(current) if current == input.accepted_revision => {}
                Some(current) => {
                    self.pending.remove(union_view_id);
                    return Err(UnionControllerError::StaleCandidate {
                        view_id: union_view_id.to_owned(),
                        input_id: input.view_id.clone(),
                        fenced: input.accepted_revision,
                        current,
                    });
                }
                None => {
                    self.pending.remove(union_view_id);
                    return Err(UnionControllerError::MissingInput {
                        view_id: union_view_id.to_owned(),
                        input_id: input.view_id.clone(),
                    });
                }
            }
        }
        let candidate = self.pending.remove(union_view_id).expect("checked");
        if !self.accepted.contains_key(union_view_id) && self.accepted.len() >= MAX_TRACKED_UNIONS {
            return Err(UnionControllerError::RegistryFull {
                maximum: MAX_TRACKED_UNIONS,
            });
        }
        let published = UnionAcceptedState {
            inputs: candidate.inputs,
            revision: candidate.union_revision,
        };
        self.accepted
            .insert(union_view_id.to_owned(), published.clone());
        Ok(published)
    }

    /// Drop a pending candidate after the runtime reports failure.
    ///
    /// Always succeeds (idempotent): a failure with nothing pending is still
    /// a failure of nothing, and accepted state is never touched.
    pub fn reject(&mut self, union_view_id: &str) {
        self.pending.remove(union_view_id);
    }

    /// Forget a union view entirely (view closed). Pending and accepted state
    /// go together; sources and inputs are unaffected — a union never owns
    /// capture.
    pub fn forget(&mut self, union_view_id: &str) {
        self.pending.remove(union_view_id);
        self.accepted.remove(union_view_id);
    }

    /// Note that an input view published a new accepted revision.
    ///
    /// Returns a refresh request when a TRACKED union references that input:
    /// pending candidates simply re-fence at publish time (see
    /// `note_union_published`), so only accepted unions refresh from here. A
    /// vanished input (`current_revision` returning `None` for it is reported
    /// by the caller as a move) likewise refreshes — the re-merge then either
    /// re-resolves or rejects with the prior union preserved.
    pub fn note_input_revision(&self, moved_view_id: &str) -> Vec<UnionRefresh> {
        self.accepted
            .iter()
            .filter_map(|(union_view_id, state)| {
                state
                    .inputs
                    .iter()
                    .any(|input| input.view_id == moved_view_id)
                    .then(|| UnionRefresh {
                        union_view_id: union_view_id.clone(),
                        union_revision: state.revision,
                        moved_view_id: moved_view_id.to_owned(),
                    })
            })
            .collect()
    }

    /// Input view IDs a union depends on, for the dependency graph walk and
    /// for archive/export freezing. `None` for a view this controller does
    /// not track (an ordinary view: a leaf).
    pub fn dependency_inputs(&self, union_view_id: &str) -> Option<Vec<String>> {
        self.accepted.get(union_view_id).map(|state| {
            state
                .inputs
                .iter()
                .map(|input| input.view_id.clone())
                .collect()
        })
    }

    /// Restore a persisted union without launching anything.
    ///
    /// Records the stored inputs as accepted under the stored union revision
    /// so the next input advance produces a refresh against a known baseline.
    /// The composition tick re-resolves the input IDs through the ordinary
    /// view-restore path; commands are never started as a side effect.
    pub fn restore(&mut self, union_view_id: &str, revision: u64, stored: StoredUnion) {
        if self.accepted.len() >= MAX_TRACKED_UNIONS && !self.accepted.contains_key(union_view_id) {
            return;
        }
        self.accepted.insert(
            union_view_id.to_owned(),
            UnionAcceptedState {
                inputs: stored.inputs,
                revision,
            },
        );
    }
}
