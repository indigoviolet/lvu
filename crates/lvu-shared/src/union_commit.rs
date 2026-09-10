//! Bounded worker-side union commit state for phase-2 remote unions.
//!
//! A window attached to a worker cannot hold the worker's
//! `lock_progress()` guards across the network, so publication fencing for
//! remote inputs moves to a worker-linearized commit: the window sends one
//! commit request naming its fully materialized, budget-reserved immutable
//! candidate, the worker verifies the frozen raw fences under its own
//! sorted guards and records an exact receipt while guards are held
//! (linearization there), and the window applies an exact receipt to a
//! still-current candidate under its own shared lock held through install.
//!
//! This module is ONLY the commit-state machine plus the fence-equality
//! predicate it needs. It owns no capture, executes no source or query
//! work, performs no I/O, spawns nothing, and holds no lock across an
//! await: every method is synchronous and every critical section is a
//! bounded map operation. In particular it is not a second evaluator —
//! verification is integer `(generation, high_watermark)` equality over the
//! same fence shape the local path checks, never row evaluation.
//!
//! Canonical-home note for [`UnionSourceFence`]: this module is the proposed
//! canonical home, relocated here from `lvu-view::union` (importing that
//! crate would cycle the graph: `lvu-shared` must not depend on `lvu-view`,
//! and `lvu-view` does not depend on `lvu-shared` today, so the reexport is
//! cycle-free). Proposed integration, NOT applied here — 5b owns those
//! files: `lvu-view/src/union.rs` replaces its struct definition with
//! `pub use lvu_shared::union_commit::UnionSourceFence;` (adding
//! `lvu-shared = { path = "../lvu-shared" }` to `lvu-view`'s dependencies).
//! Until that hunk lands, `lvu-view` keeps its identical definition; the two
//! must be unified before any wire use — this module's definition is
//! canonical and must not drift.
//!
//! Lock ordering (structural, not by discipline alone): table admission
//! takes the table mutex and drops it before any source guard is acquired;
//! the settling write-back re-acquires the table mutex with pre-copied
//! fences and never touches a guard. No path holds the table/slot mutex
//! while acquiring source guards, and no guard-typed value ever enters this
//! module — [`CommitTable::settle`] takes copied fence vectors, so a caller
//! physically cannot hold a guard inside the write-back.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// Canonical per-source fence: the generation and capture high-watermark one
/// frozen input observed, in plain data so the check is a pure function.
/// Source identities are UUID spellings, matching frozen summaries and
/// published memberships. Field-identical to the `lvu-view::union` shape it
/// relocates (see the module docs); wire-compatible by construction.
/// Canonical order is ascending `source_id` — see [`sort_fences`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnionSourceFence {
    pub source_id: String,
    pub generation: u64,
    pub high_watermark: Option<u64>,
}

/// Canonical fence order: ascending source id, matching the local path's
/// sorted guard acquisition (`union_worker` collects raw handles sorted).
pub fn sort_fences(fences: &mut [UnionSourceFence]) {
    fences.sort_by(|left, right| left.source_id.cmp(&right.source_id));
}

/// At most this many distinct raw sources per commit. Mirrors the
/// application open-source admission cap (`MAX_SOURCES = 16`,
/// `lvu-app/src/main.rs`); the worker attests only sources it owns and the
/// window names only open sources. Exceeding commits are refused, never
/// truncated. If the application cap moves, this moves with it.
pub const MAX_COMMIT_SOURCES: usize = 16;

/// At most this many unions per window in the worker receipt table,
/// enforced worker-side. Mirrors `UnionController::MAX_TRACKED_UNIONS = 128`
/// (`lvu-app/src/union_controller.rs`); the 129th distinct `union_view_id`
/// for a window is refused even though the app cap agrees. If the
/// application cap moves, this moves with it.
pub const MAX_RECEIPTS_PER_WINDOW: usize = 128;

/// At most this many windows in the receipt table. Mirrors
/// `crate::MAX_VIEWERS`; viewer admission already caps live windows, this is
/// defense-in-depth so a forged `window_id` cannot grow the table.
pub const MAX_COMMIT_WINDOWS: usize = 16;

/// Bounds for peer-supplied routing scalars on the commit path. UUID v4
/// spellings (36 bytes) and `window-<pid>` ids (~14 bytes, see
/// `default_window_id`) fit with headroom; anything larger is a protocol
/// violation, not a realloc. `MAX_WINDOW_ID_BYTES` is canonical for the
/// commit path and coordinated with the attach control plane: attach
/// validation should reference this constant rather than mint a second
/// window bound (proposed shared-file hunk to 5b — this module owns the
/// definition, no duplicate exists).
pub const MAX_NONCE_BYTES: usize = 128;
pub const MAX_WINDOW_ID_BYTES: usize = 128;

/// Fixed commitment size: domain/version-separated SHA-256 over the
/// canonical candidate traversal — length-prefixed candidate identity, each
/// input kind, ordered registration sources, revisions, generations, fences
/// and accepted-output inventory, exact retained row carriers, filter/group/
/// colour sources and every install-bearing membership vector/map in
/// deterministic key order. Computed window-side (c98 owns the traversal —
/// traversal only, no evaluator); this table treats digests opaquely as
/// equality plus this fixed length, nothing else.
pub const COMMIT_DIGEST_BYTES: usize = 32;

/// Candidate commitment: exactly [`COMMIT_DIGEST_BYTES`] bytes. Fixed-size
/// so the length contract is type-level; should a wire-compatibility
/// constraint ever force `Vec<u8>` here, it must require exactly
/// [`COMMIT_DIGEST_BYTES`] bytes.
pub type CommitDigest = [u8; COMMIT_DIGEST_BYTES];

/// View ids are already bounded (`MAX_UNION_VIEW_ID_BYTES = 256`,
/// `lvu-view/src/union.rs`); the commit path enforces the same bound.
pub const MAX_COMMIT_VIEW_ID_BYTES: usize = 256;

/// Refusal reason when the worker session moved under an attempt. Returned
/// explicitly so old handlers never time out silently.
pub const SESSION_RESET_REASON: &str = "worker session reset";

/// Refusal reason when the table-wide attempt-epoch counter exhausts
/// (`u64::MAX` admissions with no wrapping). Practically unreachable, but
/// checked: the table retires instead of reusing an epoch, so epochs stay
/// unique for the table's life and a detached-then-readmitted slot can never
/// alias an old handler's epoch (ABA).
pub const EPOCH_EXHAUSTED_REASON: &str = "commit epoch exhausted";

/// One commit attempt: a fully materialized, budget-reserved immutable
/// candidate named by routing scalars only — never rows. `frozen` must be
/// sorted ascending by `source_id` with no duplicates (canonical order, see
/// [`sort_fences`]); `digest` must bind the exact bytes the window would
/// install (raw frozen fences plus per-filtered-input kind, source set,
/// revisions and membership identity, plus the row-carrier hash), so a
/// receipt can only ever authorize this candidate.
///
/// Attempt identity is `(worker_session namespace, window_id, union_view_id,
/// monotonic candidate_generation, nonce)`. The table retains only the
/// current slot's nonce — there is no nonce history, so a nonce need only be
/// fresh per materialized candidate within its slot generation (UUID v4
/// spelling is the convention, not a lifetime-uniqueness promise): reusing a
/// nonce across generations, or a second nonce for one generation, is
/// [`CommitOutcome::NonceConflict`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitRequest {
    pub window_id: String,
    pub union_view_id: String,
    pub candidate_generation: u64,
    pub nonce: String,
    pub digest: CommitDigest,
    pub frozen: Vec<UnionSourceFence>,
}

/// Terminal-or-pending outcome of one attempt. `Pending` is observable (the
/// join path) but never stored as a verdict: the slot keeps its own pending
/// marker with the attempt epoch and settles it exactly once.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommitOutcome {
    /// Admitted and verifying; replay/query the same nonce to observe
    /// settlement. Returned immediately — there is no waiter list.
    Pending,
    /// Linearized under the worker's sorted guards; `current` is the fence
    /// the worker observed. Authorizes exactly the receipted candidate.
    Committed { current: Vec<UnionSourceFence> },
    /// The frozen fence moved before linearization; `detail` reuses the
    /// local `StaleInput` texts. Nothing installs; the window rebuilds.
    Stale { detail: String },
    /// Never admitted or never verified; `reason` is actionable
    /// (`Refused`-style). Nothing installs.
    Refused { reason: String },
    /// An older attempt for a superseded generation, or a displaced handler
    /// writing after supersession/detach. Explicit, never a silent timeout.
    Superseded,
    /// Same slot generation but a different nonce, digest, or fences — or a
    /// reused nonce on a newer generation. The slot is untouched; recovery
    /// needs a newly materialized strictly newer generation with a fresh
    /// nonce. Rejects; never replays.
    NonceConflict,
}

impl CommitOutcome {
    /// Whether this outcome settles the attempt (everything but `Pending`).
    pub fn is_terminal(&self) -> bool {
        !matches!(self, Self::Pending)
    }
}

/// The worker-recorded receipt for one attempt: the request identity plus
/// the outcome decided under the guards. The window installs only on an
/// exact `(candidate_generation, nonce, digest)` match against its retained
/// immutable candidate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitReceipt {
    pub worker_session: String,
    pub window_id: String,
    pub union_view_id: String,
    pub candidate_generation: u64,
    pub nonce: String,
    pub digest: CommitDigest,
    pub outcome: CommitOutcome,
}

impl CommitReceipt {
    /// Bind a decided outcome to the requesting attempt and session.
    pub fn answer(worker_session: &str, request: &CommitRequest, outcome: CommitOutcome) -> Self {
        Self {
            worker_session: worker_session.to_owned(),
            window_id: request.window_id.clone(),
            union_view_id: request.union_view_id.clone(),
            candidate_generation: request.candidate_generation,
            nonce: request.nonce.clone(),
            digest: request.digest,
            outcome,
        }
    }
}

/// What the table decides for one commit call. `Verify` ownership is
/// exclusive: exactly one caller per attempt epoch is told to verify, and
/// only its write-back can settle the slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommitAdmission {
    /// This caller owns the pending attempt: verify `request.frozen` under
    /// sorted source guards, then call [`CommitTable::settle`] with the
    /// copied current fences. No other caller will verify this epoch.
    /// Retain the entire guard vector across the copy and the settle call
    /// (see the caller-scope contract on `settle`).
    Verify { attempt_epoch: u64 },
    /// Do not verify. Reply with this outcome immediately.
    Answer(CommitOutcome),
}

/// Read-only status for one exact attempt. `Unknown` names only a
/// never-admitted attempt: replay the identical request/nonce on a lost
/// reply, and mint a fresh nonce only with a newly materialized strictly
/// newer generation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StatusAnswer {
    Unknown,
    Pending,
    Settled(CommitOutcome),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SlotOutcome {
    Pending,
    Settled(CommitOutcome),
}

#[derive(Clone, Debug)]
struct UnionSlot {
    highest_generation: u64,
    nonce: String,
    digest: CommitDigest,
    frozen: Vec<UnionSourceFence>,
    outcome: SlotOutcome,
    attempt_epoch: u64,
}

#[derive(Clone, Debug, Default)]
struct TableState {
    retired: bool,
    /// Table-wide monotonic attempt-epoch allocator. Never reset by detach
    /// and never wrapping (see [`TableState::alloc_epoch`]): epochs are
    /// unique for the table's life, which is what makes a detached-then-
    /// readmitted slot unable to alias a parked old handler's epoch.
    next_epoch: u64,
    windows: HashMap<String, HashMap<String, UnionSlot>>,
}

impl TableState {
    /// Allocate the next attempt epoch. `None` only at `u64::MAX`, where the
    /// caller must retire the table and refuse explicitly instead of
    /// wrapping — wrapping would let a new attempt alias a live old epoch.
    fn alloc_epoch(&mut self) -> Option<u64> {
        let epoch = self.next_epoch.checked_add(1)?;
        self.next_epoch = epoch;
        Some(epoch)
    }
}

/// Bounded worker-side receipt table for one worker session namespace.
///
/// Memory is bounded by construction: at most [`MAX_COMMIT_WINDOWS`]
/// windows, each at most [`MAX_RECEIPTS_PER_WINDOW`] union slots, each slot
/// one fingerprint plus one outcome whose fence vectors are bounded by
/// [`MAX_COMMIT_SOURCES`]. Entries never expire by timer; they leave only
/// through supersession (same slot, newer generation),
/// [`CommitTable::detach_window`] (authenticated detach), or
/// [`CommitTable::retire`] (session reset, after which the worker swaps in a
/// fresh table for the new session).
#[derive(Debug)]
pub struct CommitTable {
    session: String,
    state: Mutex<TableState>,
}

impl CommitTable {
    /// New empty receipt table for one worker session. The session string is
    /// the existing `Welcome`-bound lifetime (`RemoteSourceHandle`
    /// `worker_session` rule): receipts name it, and a new session starts a
    /// new table — old receipts never alias into it.
    pub fn new(worker_session: String) -> Self {
        Self {
            session: worker_session,
            state: Mutex::new(TableState::default()),
        }
    }

    /// The session namespace this table records for.
    pub fn session(&self) -> &str {
        &self.session
    }

    /// Retire the table on session reset. In-flight handlers holding this
    /// table get explicit outcomes afterwards (`Refused` with
    /// [`SESSION_RESET_REASON`] from `commit`/`settle`, `Unknown` from
    /// `status`) instead of timing out; the worker serves new attempts from
    /// a fresh table.
    pub fn retire(&self) {
        self.state.lock().expect("commit table poisoned").retired = true;
    }

    /// Drop one window's slots on authenticated detach (`Goodbye`/EOF).
    /// Returns whether any slots were held.
    pub fn detach_window(&self, window_id: &str) -> bool {
        self.state
            .lock()
            .expect("commit table poisoned")
            .windows
            .remove(window_id)
            .is_some()
    }

    /// Windows currently holding slots (observability only).
    pub fn window_count(&self) -> usize {
        self.state
            .lock()
            .expect("commit table poisoned")
            .windows
            .len()
    }

    /// Slots held for one window (observability only).
    pub fn union_count(&self, window_id: &str) -> usize {
        self.state
            .lock()
            .expect("commit table poisoned")
            .windows
            .get(window_id)
            .map_or(0, HashMap::len)
    }

    /// Admit one commit attempt. Brief table lock only; never acquires source
    /// guards and never blocks: the answer is immediate in every case.
    /// A `Verify` answer gives this caller exclusive ownership of the
    /// attempt epoch — it must verify under sorted source guards and then
    /// call [`CommitTable::settle`].
    pub fn commit(&self, request: &CommitRequest) -> CommitAdmission {
        if let Err(reason) = check_request_bounds(request) {
            return CommitAdmission::Answer(CommitOutcome::Refused { reason });
        }
        let mut state = self.state.lock().expect("commit table poisoned");
        if state.retired {
            return CommitAdmission::Answer(CommitOutcome::Refused {
                reason: SESSION_RESET_REASON.to_owned(),
            });
        }
        if !state.windows.contains_key(&request.window_id)
            && state.windows.len() >= MAX_COMMIT_WINDOWS
        {
            return CommitAdmission::Answer(CommitOutcome::Refused {
                reason: "window table full".to_owned(),
            });
        }
        // Read-only decision probe: it borrows nothing past this statement,
        // so the mutation phase below can allocate a fresh table-wide epoch
        // first without meeting a live slot borrow.
        let supersede = {
            let unions = state.windows.entry(request.window_id.clone()).or_default();
            match unions.get(&request.union_view_id) {
                None => {
                    if unions.len() >= MAX_RECEIPTS_PER_WINDOW {
                        return CommitAdmission::Answer(CommitOutcome::Refused {
                            reason: "union table full".to_owned(),
                        });
                    }
                    false
                }
                Some(slot) => {
                    if request.candidate_generation < slot.highest_generation {
                        return CommitAdmission::Answer(CommitOutcome::Superseded);
                    }
                    if request.candidate_generation == slot.highest_generation {
                        if same_attempt(slot, request) {
                            return match &slot.outcome {
                                SlotOutcome::Pending => {
                                    CommitAdmission::Answer(CommitOutcome::Pending)
                                }
                                SlotOutcome::Settled(outcome) => {
                                    CommitAdmission::Answer(outcome.clone())
                                }
                            };
                        }
                        return CommitAdmission::Answer(CommitOutcome::NonceConflict);
                    }
                    if request.nonce == slot.nonce {
                        return CommitAdmission::Answer(CommitOutcome::NonceConflict);
                    }
                    true
                }
            }
        };
        // Mutation phase: a fresh table-wide epoch for every newly admitted
        // attempt. Detach never resets the counter, so an identical re-admit
        // after a detach gets a strictly newer epoch and a parked old
        // handler settles as Superseded instead of aliasing the replacement
        // (ABA). Exhaustion retires the table rather than wrapping.
        let Some(epoch) = state.alloc_epoch() else {
            state.retired = true;
            return CommitAdmission::Answer(CommitOutcome::Refused {
                reason: EPOCH_EXHAUSTED_REASON.to_owned(),
            });
        };
        let unions = state
            .windows
            .get_mut(&request.window_id)
            .expect("window entry just created");
        if supersede {
            let slot = unions
                .get_mut(&request.union_view_id)
                .expect("slot decided present");
            slot.attempt_epoch = epoch;
            slot.highest_generation = request.candidate_generation;
            slot.nonce = request.nonce.clone();
            slot.digest = request.digest;
            slot.frozen.clone_from(&request.frozen);
            slot.outcome = SlotOutcome::Pending;
        } else {
            unions.insert(
                request.union_view_id.clone(),
                UnionSlot {
                    highest_generation: request.candidate_generation,
                    nonce: request.nonce.clone(),
                    digest: request.digest,
                    frozen: request.frozen.clone(),
                    outcome: SlotOutcome::Pending,
                    attempt_epoch: epoch,
                },
            );
        }
        CommitAdmission::Verify {
            attempt_epoch: epoch,
        }
    }

    /// Record the verified verdict for an admitted attempt. Takes the
    /// worker-observed `current` fences (copied out from under the caller's
    /// sorted source guards — no guard enters here) and, while holding only
    /// the table lock, writes the receipt iff this attempt is still the
    /// slot's current epoch and fingerprint. A displaced handler finds the
    /// mismatch and gets explicit [`CommitOutcome::Superseded`]; a handler
    /// finding its window detached gets `Superseded`; on a retired table it
    /// gets `Refused` with [`SESSION_RESET_REASON`]. Nothing here can
    /// overwrite a newer slot.
    ///
    /// Only a `Pending` slot transitions: settling an already-settled
    /// attempt replays its exact stored outcome without rechecking `current`,
    /// so a late duplicate write-back can neither flip a verdict nor smuggle
    /// in newer fences under an old attempt's identity.
    ///
    /// Production caller scope (unenforceable by this signature — `current`
    /// is a plain vector — so contractual, and covered by the real-guard
    /// regression tests): admit first with no guards held, then acquire and
    /// RETAIN the entire sorted source-guard vector, copy `current` out of
    /// the live guards, call `settle` while every guard is still alive, and
    /// only then drop the guards. Never wait on a publication/progress
    /// channel while holding guards (the writer's publish takes the write
    /// side); never re-pin and settle the same attempt with different fences
    /// (that observation belongs to a new candidate). Copying `current` from
    /// already-dropped guards recreates the attestation race this table
    /// exists to close.
    pub fn settle(
        &self,
        window_id: &str,
        union_view_id: &str,
        attempt_epoch: u64,
        request: &CommitRequest,
        current: Vec<UnionSourceFence>,
    ) -> CommitOutcome {
        let mut state = self.state.lock().expect("commit table poisoned");
        if state.retired {
            return CommitOutcome::Refused {
                reason: SESSION_RESET_REASON.to_owned(),
            };
        }
        let Some(slot) = state
            .windows
            .get_mut(window_id)
            .and_then(|unions| unions.get_mut(union_view_id))
        else {
            return CommitOutcome::Superseded;
        };
        if slot.attempt_epoch != attempt_epoch || !same_attempt(slot, request) {
            return CommitOutcome::Superseded;
        }
        if let SlotOutcome::Settled(outcome) = &slot.outcome {
            return outcome.clone();
        }
        let outcome = match verify_raw_fence(&slot.frozen, &current) {
            Ok(()) => CommitOutcome::Committed { current },
            Err(detail) => CommitOutcome::Stale { detail },
        };
        slot.outcome = SlotOutcome::Settled(outcome.clone());
        outcome
    }

    /// Read-only status for one exact attempt: needs
    /// `(candidate_generation, nonce, digest)` but not the frozen fences —
    /// the digest binds them. Lower than the slot's highest generation is
    /// `Superseded`; higher, or no slot at all, is `Unknown`; equal with an
    /// identical fingerprint replays `Pending` or the recorded outcome;
    /// equal with a different fingerprint is `NonceConflict`. Never mutates.
    pub fn status(
        &self,
        window_id: &str,
        union_view_id: &str,
        candidate_generation: u64,
        nonce: &str,
        digest: &CommitDigest,
    ) -> StatusAnswer {
        let state = self.state.lock().expect("commit table poisoned");
        if state.retired {
            return StatusAnswer::Unknown;
        }
        let Some(slot) = state
            .windows
            .get(window_id)
            .and_then(|unions| unions.get(union_view_id))
        else {
            return StatusAnswer::Unknown;
        };
        if candidate_generation < slot.highest_generation {
            return StatusAnswer::Settled(CommitOutcome::Superseded);
        }
        if candidate_generation > slot.highest_generation {
            return StatusAnswer::Unknown;
        }
        if slot.nonce != nonce || slot.digest != *digest {
            return StatusAnswer::Settled(CommitOutcome::NonceConflict);
        }
        match &slot.outcome {
            SlotOutcome::Pending => StatusAnswer::Pending,
            SlotOutcome::Settled(outcome) => StatusAnswer::Settled(outcome.clone()),
        }
    }
}

/// Envelope bounds for one commit request. Fences must additionally be
/// sorted ascending by `source_id` with no duplicates (canonical order —
/// the local path acquires guards sorted) and non-empty: an all-filtered
/// union has no remote raw sources and stays on the existing local
/// transaction instead of taking this path.
fn check_request_bounds(request: &CommitRequest) -> Result<(), String> {
    if request.window_id.is_empty() {
        return Err("window id empty".to_owned());
    }
    if request.window_id.len() > MAX_WINDOW_ID_BYTES {
        return Err("window id oversize".to_owned());
    }
    if request.union_view_id.len() > MAX_COMMIT_VIEW_ID_BYTES {
        return Err("union view id oversize".to_owned());
    }
    if request.nonce.len() > MAX_NONCE_BYTES {
        return Err("nonce oversize".to_owned());
    }
    // Digest length is type-level ([u8; COMMIT_DIGEST_BYTES]); no check here.
    if request.frozen.is_empty() {
        return Err("no remote raw sources".to_owned());
    }
    if request.frozen.len() > MAX_COMMIT_SOURCES {
        return Err("source fence set oversize".to_owned());
    }
    if !request
        .frozen
        .windows(2)
        .all(|pair| pair[0].source_id < pair[1].source_id)
    {
        return Err("fences must be sorted by source with no duplicates".to_owned());
    }
    Ok(())
}

/// Exact-attempt equality: same nonce, same digest, same sorted fences.
/// The digest binds the full candidate commitment (raw fences plus
/// filtered-side identity), so equality here names one materialized
/// candidate and nothing else.
fn same_attempt(slot: &UnionSlot, request: &CommitRequest) -> bool {
    slot.nonce == request.nonce && slot.digest == request.digest && slot.frozen == request.frozen
}

/// Raw-fence equality for one frozen input set against the worker-observed
/// current set: same source set with the same generation AND the same
/// high-watermark everywhere. Detail texts mirror the local
/// `verify_source_fence` stale cases (`lvu-view/src/union.rs`) minus the
/// view-id prefix the window adds when mapping to `UnionError`: advance,
/// restart, vanish, or a changed set is stale. Order-independent (matched by
/// source id); lengths must agree exactly.
pub fn verify_raw_fence(
    frozen: &[UnionSourceFence],
    current: &[UnionSourceFence],
) -> Result<(), String> {
    for fence in frozen {
        match current
            .iter()
            .find(|entry| entry.source_id == fence.source_id)
        {
            None => {
                return Err(format!("source {} is no longer published", fence.source_id));
            }
            Some(entry) if entry.generation != fence.generation => {
                return Err(format!(
                    "source {} restarted during the merge",
                    fence.source_id
                ));
            }
            Some(entry) if entry.high_watermark != fence.high_watermark => {
                return Err(format!(
                    "source {} advanced during the merge",
                    fence.source_id
                ));
            }
            Some(_) => {}
        }
    }
    if current.len() != frozen.len() {
        return Err("input source set changed during the merge".to_owned());
    }
    Ok(())
}

/// Whether worker-observed fences have moved past an installed receipt:
/// true exactly when `verify_raw_fence` fails. The window calls this shared
/// predicate at both refresh-trigger sites — with receipt-`current` vs
/// progress observed before applying the reply, and with the installed fence
/// vs progress observed after application — so both orders schedule a
/// follow-up freeze/commit through one check.
pub fn refresh_needed(installed: &[UnionSourceFence], observed: &[UnionSourceFence]) -> bool {
    verify_raw_fence(installed, observed).is_err()
}

#[cfg(test)]
mod tests {
    //! Commit-state verdicts are order/content, never timing. Threaded tests
    //! rendezvous on barriers with hang guards; every verdict holds under
    //! every interleaving the test permits.
    use super::*;
    use std::sync::{Arc, Barrier};

    fn fence(source: &str, generation: u64, high_watermark: Option<u64>) -> UnionSourceFence {
        UnionSourceFence {
            source_id: source.to_owned(),
            generation,
            high_watermark,
        }
    }

    fn sorted_fences() -> Vec<UnionSourceFence> {
        vec![fence("a", 1, Some(10)), fence("b", 1, Some(4))]
    }

    /// Deterministic 32-byte test commitment: distinct per tag, fixed-size
    /// like the production SHA-256 digests (whose computation lives
    /// window-side; the table only compares).
    fn digest(tag: &str) -> CommitDigest {
        assert!(!tag.is_empty(), "digest tag must be non-empty");
        let bytes = tag.as_bytes();
        let mut out = [0u8; COMMIT_DIGEST_BYTES];
        for (index, slot) in out.iter_mut().enumerate() {
            *slot = bytes[index % bytes.len()].wrapping_add(index as u8);
        }
        out
    }

    fn commit_request(
        window: &str,
        union_view: &str,
        generation: u64,
        nonce: &str,
        fences: Vec<UnionSourceFence>,
    ) -> CommitRequest {
        CommitRequest {
            window_id: window.to_owned(),
            union_view_id: union_view.to_owned(),
            candidate_generation: generation,
            nonce: nonce.to_owned(),
            digest: digest(nonce),
            frozen: fences,
        }
    }

    fn table(session: &str) -> CommitTable {
        CommitTable::new(session.to_owned())
    }

    /// Drive one admitted attempt to settlement: the test is the handler —
    /// admit, copy `current` (as if from under sorted source guards), settle.
    fn settle_as(
        table: &CommitTable,
        request: &CommitRequest,
        epoch: u64,
        current: Vec<UnionSourceFence>,
    ) -> CommitOutcome {
        table.settle(
            &request.window_id,
            &request.union_view_id,
            epoch,
            request,
            current,
        )
    }

    #[test]
    fn fence_equality_mirrors_local_cases() {
        let frozen = sorted_fences();
        assert!(verify_raw_fence(&frozen, &frozen).is_ok());
        assert!(verify_raw_fence(&[], &[]).is_ok());
        let mut advanced = frozen.clone();
        advanced[0].high_watermark = Some(11);
        assert_eq!(
            verify_raw_fence(&frozen, &advanced),
            Err("source a advanced during the merge".to_owned())
        );
        let mut restarted = frozen.clone();
        restarted[1].generation = 2;
        assert_eq!(
            verify_raw_fence(&frozen, &restarted),
            Err("source b restarted during the merge".to_owned())
        );
        let vanished = vec![fence("a", 1, Some(10))];
        assert_eq!(
            verify_raw_fence(&frozen, &vanished),
            Err("source b is no longer published".to_owned())
        );
        let mut added = frozen.clone();
        added.push(fence("c", 1, None));
        assert_eq!(
            verify_raw_fence(&frozen, &added),
            Err("input source set changed during the merge".to_owned())
        );
        // Order-independent: same set in a different order still verifies.
        let mut reordered = frozen.clone();
        reordered.reverse();
        assert!(verify_raw_fence(&frozen, &reordered).is_ok());
    }

    #[test]
    fn commit_admits_settles_and_replays_committed() {
        let table = table("session-a");
        let request = commit_request("w-1", "u-1", 7, "n-1", sorted_fences());
        let CommitAdmission::Verify { attempt_epoch } = table.commit(&request) else {
            panic!("first commit must be admitted for verification");
        };
        // Duplicate exact attempt while pending joins: Pending immediately,
        // no waiter list, no second verification.
        assert_eq!(
            table.commit(&request),
            CommitAdmission::Answer(CommitOutcome::Pending)
        );
        assert_eq!(
            table.status("w-1", "u-1", 7, "n-1", &request.digest),
            StatusAnswer::Pending
        );
        let outcome = settle_as(&table, &request, attempt_epoch, sorted_fences());
        assert_eq!(
            outcome,
            CommitOutcome::Committed {
                current: sorted_fences()
            }
        );
        // Terminal replay is byte-identical for commit and status alike.
        assert_eq!(
            table.commit(&request),
            CommitAdmission::Answer(outcome.clone())
        );
        assert_eq!(
            table.status("w-1", "u-1", 7, "n-1", &request.digest),
            StatusAnswer::Settled(outcome)
        );
        let receipt = CommitReceipt::answer("session-a", &request, CommitOutcome::Pending);
        assert_eq!(receipt.worker_session, "session-a");
        assert_eq!(receipt.nonce, "n-1");
        assert!(!CommitOutcome::Pending.is_terminal());
        assert!(CommitOutcome::Superseded.is_terminal());
    }

    #[test]
    fn lower_generation_is_superseded_on_both_paths() {
        let table = table("session-a");
        let newer = commit_request("w-1", "u-1", 9, "n-9", sorted_fences());
        let CommitAdmission::Verify { attempt_epoch } = table.commit(&newer) else {
            panic!("newer commit must be admitted");
        };
        settle_as(&table, &newer, attempt_epoch, sorted_fences());
        let older = commit_request("w-1", "u-1", 8, "n-8", sorted_fences());
        assert_eq!(
            table.commit(&older),
            CommitAdmission::Answer(CommitOutcome::Superseded)
        );
        assert_eq!(
            table.status("w-1", "u-1", 8, "n-8", &older.digest),
            StatusAnswer::Settled(CommitOutcome::Superseded)
        );
        // The slot still belongs to generation 9.
        assert_eq!(
            table.status("w-1", "u-1", 9, "n-9", &newer.digest),
            StatusAnswer::Settled(CommitOutcome::Committed {
                current: sorted_fences()
            })
        );
    }

    #[test]
    fn changed_fingerprint_under_same_nonce_conflicts_without_touching_slot() {
        let table = table("session-a");
        let first = commit_request("w-1", "u-1", 7, "n-7", sorted_fences());
        let CommitAdmission::Verify { .. } = table.commit(&first) else {
            panic!("first commit must be admitted");
        };
        // Same nonce, changed digest.
        let mut changed_digest = first.clone();
        changed_digest.digest = digest("digest-other");
        assert_eq!(
            table.commit(&changed_digest),
            CommitAdmission::Answer(CommitOutcome::NonceConflict)
        );
        // Same nonce, changed fences (digest rebound accordingly).
        let mut changed_fences = first.clone();
        changed_fences.frozen[0].high_watermark = Some(99);
        changed_fences.digest = digest("digest-fences");
        assert_eq!(
            table.commit(&changed_fences),
            CommitAdmission::Answer(CommitOutcome::NonceConflict)
        );
        // Same generation, different nonce: also a conflict, never a replay.
        let rival = commit_request("w-1", "u-1", 7, "n-rival", sorted_fences());
        assert_eq!(
            table.commit(&rival),
            CommitAdmission::Answer(CommitOutcome::NonceConflict)
        );
        // The slot still holds the first attempt, still pending.
        assert_eq!(
            table.status("w-1", "u-1", 7, "n-7", &first.digest),
            StatusAnswer::Pending
        );
        // Status with a mismatched digest at the same generation conflicts.
        assert_eq!(
            table.status("w-1", "u-1", 7, "n-7", &digest("digest-other")),
            StatusAnswer::Settled(CommitOutcome::NonceConflict)
        );
    }

    /// A single changed digest byte under the same nonce is a conflict on
    /// both paths — the digest binds the whole commitment, so any change is
    /// a different candidate, never a replay.
    #[test]
    fn same_nonce_one_byte_digest_change_conflicts() {
        let table = table("session-a");
        let first = commit_request("w-1", "u-1", 7, "n-7", sorted_fences());
        let CommitAdmission::Verify { .. } = table.commit(&first) else {
            panic!("first commit must be admitted");
        };
        let mut off_by_one = first.clone();
        off_by_one.digest[0] ^= 0x01;
        assert_ne!(off_by_one.digest, first.digest);
        assert_eq!(
            table.commit(&off_by_one),
            CommitAdmission::Answer(CommitOutcome::NonceConflict)
        );
        assert_eq!(
            table.status("w-1", "u-1", 7, "n-7", &off_by_one.digest),
            StatusAnswer::Settled(CommitOutcome::NonceConflict)
        );
        assert_eq!(
            table.status("w-1", "u-1", 7, "n-7", &first.digest),
            StatusAnswer::Pending
        );
    }

    #[test]
    fn newer_generation_supersedes_and_old_write_drops() {
        let table = table("session-a");
        let old = commit_request("w-1", "u-1", 7, "n-7", sorted_fences());
        let CommitAdmission::Verify {
            attempt_epoch: old_epoch,
        } = table.commit(&old)
        else {
            panic!("old commit must be admitted");
        };
        // The slow handler is parked pre-verification here (the test simply
        // delays its settle call): a strictly newer fresh candidate arrives.
        let fresh = commit_request("w-1", "u-1", 8, "n-8", sorted_fences());
        let CommitAdmission::Verify {
            attempt_epoch: fresh_epoch,
        } = table.commit(&fresh)
        else {
            panic!("fresh commit must supersede and be admitted");
        };
        assert_ne!(old_epoch, fresh_epoch);
        // The displaced handler's write-back finds the epoch mismatch and is
        // dropped with an explicit outcome — it never overwrites the slot.
        assert_eq!(
            settle_as(&table, &old, old_epoch, sorted_fences()),
            CommitOutcome::Superseded
        );
        assert_eq!(
            table.status("w-1", "u-1", 7, "n-7", &old.digest),
            StatusAnswer::Settled(CommitOutcome::Superseded)
        );
        // The newer attempt still settles normally.
        assert_eq!(
            settle_as(&table, &fresh, fresh_epoch, sorted_fences()),
            CommitOutcome::Committed {
                current: sorted_fences()
            }
        );
        // Nonce reuse across generations is a conflict, not a supersede.
        let reused = commit_request("w-1", "u-1", 9, "n-8", sorted_fences());
        assert_eq!(
            table.commit(&reused),
            CommitAdmission::Answer(CommitOutcome::NonceConflict)
        );
    }

    /// Detach/re-admit ABA: epochs come from one table-wide monotonic
    /// counter that detach never resets, so an identical re-admit gets a
    /// strictly newer epoch and the parked old handler settles as
    /// Superseded while the replacement stays Pending.
    #[test]
    fn detached_readmit_keeps_epochs_unique_aba_rejected() {
        let table = table("session-a");
        let old = commit_request("w-1", "u-1", 7, "n-7", sorted_fences());
        let CommitAdmission::Verify {
            attempt_epoch: old_epoch,
        } = table.commit(&old)
        else {
            panic!("old commit must be admitted");
        };
        // The old handler parks pre-verification; the window detaches and
        // the identical request is admitted anew into a fresh slot.
        assert!(table.detach_window("w-1"));
        let same = old.clone();
        let CommitAdmission::Verify {
            attempt_epoch: fresh_epoch,
        } = table.commit(&same)
        else {
            panic!("identical re-admit after detach must be admitted anew");
        };
        assert_ne!(
            old_epoch, fresh_epoch,
            "detach must not recycle the epoch (ABA)"
        );
        // The parked old write-back finds the epoch mismatch: explicit
        // Superseded, and the replacement is untouched — still Pending.
        assert_eq!(
            settle_as(&table, &old, old_epoch, sorted_fences()),
            CommitOutcome::Superseded
        );
        assert_eq!(
            table.status("w-1", "u-1", 7, "n-7", &same.digest),
            StatusAnswer::Pending
        );
        // The replacement still settles normally afterwards.
        assert_eq!(
            settle_as(&table, &same, fresh_epoch, sorted_fences()),
            CommitOutcome::Committed {
                current: sorted_fences()
            }
        );
    }

    /// Only Pending→terminal is permitted: settling an already-settled
    /// attempt replays its exact stored outcome without rechecking `current`,
    /// so a late duplicate write-back can neither flip the verdict nor smuggle
    /// newer fences under the old attempt's identity.
    #[test]
    fn settled_attempt_replays_exact_outcome_without_recheck() {
        let table = table("session-a");
        let request = commit_request("w-1", "u-1", 5, "n-5", sorted_fences());
        let CommitAdmission::Verify { attempt_epoch } = table.commit(&request) else {
            panic!("commit must be admitted");
        };
        let committed = settle_as(&table, &request, attempt_epoch, sorted_fences());
        assert_eq!(
            committed,
            CommitOutcome::Committed {
                current: sorted_fences()
            }
        );
        // The world moved on, but the same attempt settling again replays the
        // stored receipt exactly — no re-verification against the new fences.
        let mut advanced = sorted_fences();
        advanced[0].high_watermark = Some(999);
        assert_eq!(
            settle_as(&table, &request, attempt_epoch, advanced),
            CommitOutcome::Committed {
                current: sorted_fences()
            }
        );
        assert_eq!(
            table.status("w-1", "u-1", 5, "n-5", &request.digest),
            StatusAnswer::Settled(CommitOutcome::Committed {
                current: sorted_fences()
            })
        );
    }

    /// The epoch counter is checked, never wrapping: at `u64::MAX` the table
    /// retires and every path answers explicitly from then on.
    #[test]
    fn epoch_exhaustion_retires_without_wrapping() {
        let table = table("session-a");
        {
            let mut state = table.state.lock().expect("commit table poisoned");
            state.next_epoch = u64::MAX;
        }
        let request = commit_request("w-1", "u-1", 1, "n-1", sorted_fences());
        assert_eq!(
            table.commit(&request),
            CommitAdmission::Answer(CommitOutcome::Refused {
                reason: EPOCH_EXHAUSTED_REASON.to_owned()
            })
        );
        assert_eq!(
            table.status("w-1", "u-1", 1, "n-1", &request.digest),
            StatusAnswer::Unknown
        );
        let later = commit_request("w-1", "u-2", 1, "n-2", sorted_fences());
        assert_eq!(
            table.commit(&later),
            CommitAdmission::Answer(CommitOutcome::Refused {
                reason: SESSION_RESET_REASON.to_owned()
            })
        );
    }

    #[test]
    fn status_unknown_only_for_never_admitted() {
        let table = table("session-a");
        assert_eq!(
            table.status("w-1", "u-1", 7, "n-7", &digest("n-7")),
            StatusAnswer::Unknown
        );
        let request = commit_request("w-1", "u-1", 7, "n-7", sorted_fences());
        let CommitAdmission::Verify { attempt_epoch } = table.commit(&request) else {
            panic!("commit must be admitted");
        };
        // Higher than the slot's highest: Unknown, never Superseded.
        assert_eq!(
            table.status("w-1", "u-1", 8, "n-8", &digest("n-8")),
            StatusAnswer::Unknown
        );
        settle_as(&table, &request, attempt_epoch, sorted_fences());
        // A lost reply replays the identical nonce; a fresh generation needs
        // a newly materialized candidate, which the window supplies — the
        // table only ever answers what was admitted.
        assert!(matches!(
            table.status("w-1", "u-1", 7, "n-7", &request.digest),
            StatusAnswer::Settled(CommitOutcome::Committed { .. })
        ));
    }

    #[test]
    fn detach_clears_and_retire_answers_explicitly() {
        let table = table("session-a");
        let request = commit_request("w-1", "u-1", 7, "n-7", sorted_fences());
        let CommitAdmission::Verify { attempt_epoch } = table.commit(&request) else {
            panic!("commit must be admitted");
        };
        assert!(table.detach_window("w-1"));
        assert!(!table.detach_window("w-1"));
        assert_eq!(
            table.status("w-1", "u-1", 7, "n-7", &request.digest),
            StatusAnswer::Unknown
        );
        // The detached handler's write-back is explicit, not a silent drop.
        assert_eq!(
            settle_as(&table, &request, attempt_epoch, sorted_fences()),
            CommitOutcome::Superseded
        );
        // The same exact request afterwards is never-admitted: admitted anew.
        let CommitAdmission::Verify { .. } = table.commit(&request) else {
            panic!("post-detach commit must be admitted anew");
        };
        table.retire();
        // Retired tables answer explicitly on every path.
        let next = commit_request("w-1", "u-2", 1, "n-x", sorted_fences());
        assert_eq!(
            table.commit(&next),
            CommitAdmission::Answer(CommitOutcome::Refused {
                reason: SESSION_RESET_REASON.to_owned()
            })
        );
        assert_eq!(
            settle_as(&table, &request, attempt_epoch, sorted_fences()),
            CommitOutcome::Refused {
                reason: SESSION_RESET_REASON.to_owned()
            }
        );
        assert_eq!(
            table.status("w-1", "u-1", 7, "n-7", &request.digest),
            StatusAnswer::Unknown
        );
    }

    #[test]
    fn envelope_bounds_refuse_explicitly() {
        let table = table("session-a");
        let base = commit_request("w-1", "u-1", 1, "n-1", sorted_fences());
        let mut empty_window = base.clone();
        empty_window.window_id = String::new();
        assert_eq!(
            table.commit(&empty_window),
            CommitAdmission::Answer(CommitOutcome::Refused {
                reason: "window id empty".to_owned()
            })
        );
        let mut oversize_union = base.clone();
        oversize_union.union_view_id = "u".repeat(MAX_COMMIT_VIEW_ID_BYTES + 1);
        assert!(matches!(
            table.commit(&oversize_union),
            CommitAdmission::Answer(CommitOutcome::Refused { .. })
        ));
        let mut oversize_nonce = base.clone();
        oversize_nonce.nonce = "n".repeat(MAX_NONCE_BYTES + 1);
        assert!(matches!(
            table.commit(&oversize_nonce),
            CommitAdmission::Answer(CommitOutcome::Refused { .. })
        ));
        let mut oversize_window = base.clone();
        oversize_window.window_id = "w".repeat(MAX_WINDOW_ID_BYTES + 1);
        assert!(matches!(
            table.commit(&oversize_window),
            CommitAdmission::Answer(CommitOutcome::Refused { .. })
        ));
        // Digest length is type-level ([u8; COMMIT_DIGEST_BYTES]): an
        // oversize digest is unrepresentable, so no refusal case exists.
        let empty = commit_request("w-1", "u-1", 1, "n-e", Vec::new());
        assert_eq!(
            table.commit(&empty),
            CommitAdmission::Answer(CommitOutcome::Refused {
                reason: "no remote raw sources".to_owned()
            })
        );
        let mut too_many = Vec::new();
        for index in 0..=MAX_COMMIT_SOURCES {
            too_many.push(fence(&format!("s-{index:03}"), 1, Some(0)));
        }
        let wide = commit_request("w-1", "u-1", 1, "n-w", too_many);
        assert!(matches!(
            table.commit(&wide),
            CommitAdmission::Answer(CommitOutcome::Refused { .. })
        ));
        // Unsorted and duplicate fences are refused: canonical order is what
        // makes fingerprint equality meaningful.
        let mut unsorted = sorted_fences();
        unsorted.reverse();
        let reversed = commit_request("w-1", "u-1", 1, "n-r", unsorted);
        assert!(matches!(
            table.commit(&reversed),
            CommitAdmission::Answer(CommitOutcome::Refused { .. })
        ));
        let duplicated = commit_request(
            "w-1",
            "u-1",
            1,
            "n-d",
            vec![fence("a", 1, Some(1)), fence("a", 1, Some(1))],
        );
        assert!(matches!(
            table.commit(&duplicated),
            CommitAdmission::Answer(CommitOutcome::Refused { .. })
        ));
        // Refusals touch nothing: a valid commit afterwards is admitted.
        let CommitAdmission::Verify { .. } = table.commit(&base) else {
            panic!("valid commit after refusals must be admitted");
        };
    }

    /// Wire length contract: 31- and 33-element digest arrays are rejected;
    /// exactly 32 round-trips for both request and receipt.
    #[test]
    fn serde_rejects_off_by_one_digest_lengths() {
        let request = commit_request("w-1", "u-1", 7, "n-7", sorted_fences());
        let wire = serde_json::to_value(&request).expect("serialize request");
        for len in [31usize, 33usize] {
            let mut tampered = wire.clone();
            tampered["digest"] = serde_json::Value::Array(
                (0..len)
                    .map(|byte| serde_json::Value::from(byte as u64))
                    .collect(),
            );
            assert!(
                serde_json::from_value::<CommitRequest>(tampered).is_err(),
                "{len}-byte digest must be rejected"
            );
        }
        let request_back: CommitRequest =
            serde_json::from_value(wire).expect("32-byte digest round-trips");
        assert_eq!(request_back, request);
        let receipt = CommitReceipt::answer(
            "session-a",
            &request,
            CommitOutcome::Committed {
                current: sorted_fences(),
            },
        );
        let receipt_back: CommitReceipt =
            serde_json::from_value(serde_json::to_value(&receipt).expect("serialize receipt"))
                .expect("receipt round-trips");
        assert_eq!(receipt_back, receipt);
    }

    #[test]
    fn worker_side_union_cap_binds_per_window() {
        let table = table("session-a");
        for index in 0..MAX_RECEIPTS_PER_WINDOW {
            let request = commit_request(
                "w-1",
                &format!("u-{index:03}"),
                1,
                &format!("n-{index:03}"),
                sorted_fences(),
            );
            assert!(
                matches!(table.commit(&request), CommitAdmission::Verify { .. }),
                "union {index} must be admitted"
            );
        }
        assert_eq!(table.union_count("w-1"), MAX_RECEIPTS_PER_WINDOW);
        let overflow = commit_request("w-1", "u-129", 1, "n-129", sorted_fences());
        assert_eq!(
            table.commit(&overflow),
            CommitAdmission::Answer(CommitOutcome::Refused {
                reason: "union table full".to_owned()
            })
        );
        // Another window has its own budget.
        let other = commit_request("w-2", "u-129", 1, "n-129", sorted_fences());
        assert!(matches!(
            table.commit(&other),
            CommitAdmission::Verify { .. }
        ));
        assert_eq!(table.window_count(), 2);
    }

    #[test]
    fn refresh_triggers_cover_before_reply_and_after_application() {
        let installed = sorted_fences();
        // Before the reply is applied: receipt `current` already committed,
        // but fresher worker progress arrived first — follow-up required.
        let mut observed = installed.clone();
        observed[1].high_watermark = Some(5);
        assert!(refresh_needed(&installed, &observed));
        // After application: the installed union is behind newly observed
        // progress — same predicate, second call site, same verdict.
        assert!(refresh_needed(&installed, &observed));
        assert!(!refresh_needed(&installed, &installed));
        assert!(!refresh_needed(&observed, &observed));
    }

    /// Two racing identical commits: exactly one verifies, the other joins
    /// with an immediate `Pending` — no waiter list, no second verification.
    /// The barrier start makes the race real; the verdict multiset holds
    /// under every interleaving.
    #[test]
    fn identical_race_admits_once_and_joins() {
        let table = Arc::new(table("session-a"));
        let request = commit_request("w-1", "u-1", 7, "n-7", sorted_fences());
        let start = Arc::new(Barrier::new(3));
        let mut threads = Vec::new();
        for _ in 0..2 {
            let table = table.clone();
            let request = request.clone();
            let start = start.clone();
            threads.push(std::thread::spawn(move || {
                start.wait();
                table.commit(&request)
            }));
        }
        start.wait();
        let mut verified = 0;
        let mut pending = 0;
        for thread in threads {
            match thread.join().expect("committer thread") {
                CommitAdmission::Verify { .. } => verified += 1,
                CommitAdmission::Answer(CommitOutcome::Pending) => pending += 1,
                other => panic!("unexpected race outcome: {other:?}"),
            }
        }
        assert_eq!((verified, pending), (1, 1));
    }

    /// Slow-handler supersession across threads: the parked first attempt can
    /// neither settle the slot nor block the second, and its write-back is
    /// an explicit `Superseded`.
    #[test]
    fn threaded_supersession_drops_parked_write() {
        let table = Arc::new(table("session-a"));
        let old = commit_request("w-1", "u-1", 7, "n-7", sorted_fences());
        let CommitAdmission::Verify {
            attempt_epoch: old_epoch,
        } = table.commit(&old)
        else {
            panic!("old commit must be admitted");
        };
        let parked = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let settled = {
            let table = table.clone();
            let old = old.clone();
            let parked = parked.clone();
            let release = release.clone();
            std::thread::spawn(move || {
                parked.wait();
                release.wait();
                settle_as(&table, &old, old_epoch, sorted_fences())
            })
        };
        parked.wait();
        let fresh = commit_request("w-1", "u-1", 8, "n-8", sorted_fences());
        let CommitAdmission::Verify { .. } = table.commit(&fresh) else {
            panic!("fresh commit must supersede");
        };
        release.wait();
        assert_eq!(
            settled.join().expect("parked handler"),
            CommitOutcome::Superseded
        );
    }

    // ---- real-source-guard tests: fences below are copied out of actual
    // `SourceHandle::lock_progress()` guards on live captures. Every settle
    // runs inside `with_pinned`, i.e. with the entire sorted guard vector
    // retained alive across the copy and the settle call — the production
    // handler order (admit with no guards held, retain guards, copy, settle
    // while alive, drop, and only then wait on progress). ----

    #[cfg(test)]
    mod real_guard_tests {
        use super::super::*;
        use super::digest;
        use lvu_core::{Acquisition, SourceDefinition, SourceId};
        use lvu_ingest::publish_probe::{self, PrePublishObservation};
        use lvu_ingest::{RuntimeConfig, RuntimeError, SourceHandle, SourceManager};
        use std::collections::BTreeMap;
        use std::sync::Arc;
        use std::time::Duration;

        fn file_definition(id: SourceId, path: &std::path::Path, follow: bool) -> SourceDefinition {
            SourceDefinition {
                schema_version: 1,
                id,
                name: "commit fixture".into(),
                acquisition: Acquisition::File {
                    path: path.to_owned(),
                    follow,
                },
                identity_hints: BTreeMap::new(),
                retention: None,
            }
        }

        fn small_config() -> RuntimeConfig {
            let mut config = RuntimeConfig::default();
            config.acquisition.channel_capacity = 2;
            config.acquisition.read_chunk_bytes = 16;
            config.acquisition.maximum_record_bytes = 64;
            config.acquisition.partial_flush_interval = Duration::from_millis(20);
            config.writer_queue_capacity = 2;
            config.batch_records = 4;
            config.sync_every_records = 8;
            config.sync_interval = Duration::from_millis(20);
            config.graceful_stop_deadline = Duration::from_secs(3);
            config
        }

        async fn wait_for_records(handle: &SourceHandle, records: u64) {
            let mut progress = handle.subscribe();
            tokio::time::timeout(Duration::from_secs(30), async {
                loop {
                    if progress.borrow().records >= records {
                        return;
                    }
                    progress
                        .changed()
                        .await
                        .expect("capture progress channel closed");
                }
            })
            .await
            .expect("capture records timed out");
        }

        /// A `follow: false` source exits on its own once journaled, so
        /// `stop` may legitimately find nothing active; only a hang or an
        /// unexpected error fails the test.
        async fn stop_quietly(handle: &SourceHandle, what: &'static str) {
            match tokio::time::timeout(Duration::from_secs(10), handle.stop()).await {
                Err(_) => panic!("{what} stop hung"),
                Ok(Err(RuntimeError::NotActive | RuntimeError::Closed)) => {}
                Ok(Err(error)) => panic!("{what} unexpected stop error: {error:?}"),
                Ok(Ok(_)) => {}
            }
        }

        /// Pin the entire sorted guard vector and keep it alive for the
        /// closure: fences are copied out of live guards, and the closure
        /// (which performs the settle) runs while every guard is still held.
        /// Returning only copied fences — dropping each guard inside the
        /// iterator — would recreate the attestation race this table closes,
        /// so the helper structurally forbids it: the only way to settle
        /// here is with the guards alive.
        fn with_pinned<R>(
            handles: &[SourceHandle],
            settle: impl FnOnce(Vec<UnionSourceFence>) -> R,
        ) -> R {
            let mut ordered: Vec<&SourceHandle> = handles.iter().collect();
            ordered.sort_by_key(|handle| handle.source_id().0);
            let guards: Vec<_> = ordered
                .iter()
                .map(|handle| handle.lock_progress())
                .collect();
            let fences = ordered
                .iter()
                .zip(guards.iter())
                .map(|(handle, guard)| UnionSourceFence {
                    source_id: handle.source_id().0.to_string(),
                    generation: guard.generation(),
                    high_watermark: guard.high_watermark().map(|record| record.sequence),
                })
                .collect();
            let outcome = settle(fences);
            drop(guards);
            outcome
        }

        /// Brief freeze observation: copy fences and release immediately.
        /// Used only to build the pre-image a commit admits; the verifying
        /// observation always goes through [`with_pinned`] with guards held
        /// across the settle.
        fn freeze_copy(handles: &[SourceHandle]) -> Vec<UnionSourceFence> {
            with_pinned(handles, |fences| fences)
        }

        #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn real_guards_commit_and_diverged_stale() {
            let root = tempfile::tempdir().expect("capture root");
            let manager =
                SourceManager::new(root.path().join("capture"), small_config()).expect("manager");
            let first_path = root.path().join("first.log");
            let second_path = root.path().join("second.log");
            std::fs::write(&first_path, "a-1\na-2\na-3\n").expect("fixture");
            std::fs::write(&second_path, "b-1\nb-2\nb-3\n").expect("fixture");
            let first_id = SourceId(uuid::Uuid::from_u128(0xC001));
            let second_id = SourceId(uuid::Uuid::from_u128(0xC002));
            let first = manager
                .start(file_definition(first_id, &first_path, false))
                .await
                .expect("start first");
            let second = manager
                .start(file_definition(second_id, &second_path, false))
                .await
                .expect("start second");
            wait_for_records(&first, 3).await;
            wait_for_records(&second, 3).await;

            let table = CommitTable::new("worker-session-1".to_owned());
            let handles = [first.clone(), second.clone()];
            let frozen = freeze_copy(&handles);
            assert_eq!(frozen.len(), 2);
            let request = CommitRequest {
                window_id: "w-1".to_owned(),
                union_view_id: "u-1".to_owned(),
                candidate_generation: 7,
                nonce: "n-real-7".to_owned(),
                digest: digest("digest-real-7"),
                frozen: frozen.clone(),
            };
            let CommitAdmission::Verify { attempt_epoch } = table.commit(&request) else {
                panic!("real-guard commit must be admitted");
            };
            // Settle with guards retained alive across copy and settle: a
            // stable capture commits.
            let outcome = with_pinned(&handles, |current| {
                assert_eq!(current, frozen);
                table.settle("w-1", "u-1", attempt_epoch, &request, current)
            });
            assert!(matches!(outcome, CommitOutcome::Committed { .. }));
            let receipt = CommitReceipt::answer(
                table.session(),
                &request,
                CommitOutcome::Committed {
                    current: frozen.clone(),
                },
            );
            assert_eq!(receipt.worker_session, "worker-session-1");
            assert_eq!(receipt.candidate_generation, 7);

            // A diverged frozen set against the same live guards is stale
            // with the local detail text.
            let mut diverged = frozen.clone();
            diverged[0].high_watermark = Some(diverged[0].high_watermark.unwrap_or(0) + 100);
            let stale_request = CommitRequest {
                window_id: "w-1".to_owned(),
                union_view_id: "u-1".to_owned(),
                candidate_generation: 8,
                nonce: "n-real-8".to_owned(),
                digest: digest("digest-real-8"),
                frozen: diverged,
            };
            let CommitAdmission::Verify {
                attempt_epoch: stale_epoch,
            } = table.commit(&stale_request)
            else {
                panic!("diverged commit must be admitted (newer generation)");
            };
            let outcome = with_pinned(&handles, |live| {
                table.settle("w-1", "u-1", stale_epoch, &stale_request, live)
            });
            assert_eq!(
                outcome,
                CommitOutcome::Stale {
                    detail: format!("source {} advanced during the merge", frozen[0].source_id)
                }
            );

            stop_quietly(&first, "first").await;
            stop_quietly(&second, "second").await;
        }

        /// One followed source, a writer thread appending lines, and three
        /// committer threads driving admit/pin/settle through real guards.
        /// Verdicts hold under every interleaving: each settle compares the
        /// fences it was given, generations resolve to the highest admitted,
        /// and nothing deadlocks (outer hang guard fails loudly otherwise).
        #[tokio::test(flavor = "multi_thread", worker_threads = 6)]
        async fn concurrent_committers_and_writer_stay_consistent() {
            let root = tempfile::tempdir().expect("capture root");
            let manager =
                SourceManager::new(root.path().join("capture"), small_config()).expect("manager");
            let path = root.path().join("live.log");
            std::fs::write(&path, "r-0\nr-1\nr-2\n").expect("fixture");
            let source_id = SourceId(uuid::Uuid::from_u128(0xC003));
            let handle = manager
                .start(file_definition(source_id, &path, true))
                .await
                .expect("start followed source");
            wait_for_records(&handle, 3).await;

            let table = Arc::new(CommitTable::new("worker-session-2".to_owned()));
            let writer_done = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let writer = {
                let path = path.clone();
                let writer_done = writer_done.clone();
                std::thread::spawn(move || {
                    use std::io::Write;
                    for index in 3..23u64 {
                        let mut file = std::fs::OpenOptions::new()
                            .append(true)
                            .open(&path)
                            .expect("append fixture");
                        writeln!(file, "r-{index}").expect("append line");
                    }
                    writer_done.store(true, std::sync::atomic::Ordering::Release);
                })
            };
            let mut workers = Vec::new();
            for worker in 0..3u64 {
                let table = table.clone();
                let handle = handle.clone();
                let writer_done = writer_done.clone();
                let nonce = format!("n-live-{worker}");
                workers.push(std::thread::spawn(move || {
                    let generation = 50 + worker;
                    let mut committed = 0u32;
                    let mut settled_total = 0u32;
                    for _ in 0..10u32 {
                        // Freeze pre-image (brief pin), admit with no guards
                        // held, then verify with the guard vector retained
                        // alive across the copy and the settle.
                        let frozen = freeze_copy(std::slice::from_ref(&handle));
                        let request = CommitRequest {
                            window_id: "w-live".to_owned(),
                            union_view_id: "u-live".to_owned(),
                            candidate_generation: generation,
                            nonce: nonce.clone(),
                            digest: digest(&nonce),
                            frozen,
                        };
                        match table.commit(&request) {
                            CommitAdmission::Verify { attempt_epoch } => {
                                let outcome =
                                    with_pinned(std::slice::from_ref(&handle), |current| {
                                        let echoed = current.clone();
                                        let outcome = table.settle(
                                            "w-live",
                                            "u-live",
                                            attempt_epoch,
                                            &request,
                                            current,
                                        );
                                        if let CommitOutcome::Committed { current } = &outcome {
                                            assert_eq!(current, &echoed);
                                        }
                                        outcome
                                    });
                                match outcome {
                                    CommitOutcome::Committed { .. } => committed += 1,
                                    CommitOutcome::Stale { .. } | CommitOutcome::Superseded => {}
                                    other => panic!("unexpected settle outcome: {other:?}"),
                                }
                                settled_total += 1;
                            }
                            CommitAdmission::Answer(outcome) => {
                                assert!(
                                    matches!(
                                        outcome,
                                        CommitOutcome::Pending
                                            | CommitOutcome::Committed { .. }
                                            | CommitOutcome::Stale { .. }
                                            | CommitOutcome::Superseded
                                            | CommitOutcome::NonceConflict
                                    ),
                                    "unexpected admission answer: {outcome:?}"
                                );
                            }
                        }
                        if writer_done.load(std::sync::atomic::Ordering::Acquire) {
                            // Keep racing a little past the last append so
                            // late publishes meet live commits.
                            std::thread::yield_now();
                        }
                    }
                    (worker, committed, settled_total)
                }));
            }
            let outcome = tokio::time::timeout(Duration::from_secs(60), async {
                writer.join().expect("writer thread");
                let mut joined = Vec::new();
                for thread in workers {
                    joined.push(
                        tokio::task::spawn_blocking(|| thread.join().expect("worker thread"))
                            .await
                            .expect("join task"),
                    );
                }
                joined
            })
            .await
            .expect("concurrent commit test hung");
            assert_eq!(outcome.len(), 3);
            // Generation 52 is the highest any thread names, and no higher
            // generation exists, so its first commit is always admitted and
            // it always settles at least once regardless of interleaving.
            let top = outcome
                .iter()
                .find(|(worker, _, _)| *worker == 2)
                .expect("gen-52 worker");
            assert!(top.2 >= 1, "highest generation must settle: {outcome:?}");
            wait_for_records(&handle, 23).await;
            // Highest admitted generation wins regardless of interleaving.
            assert_eq!(
                table.status("w-live", "u-live", 53, "n-live-9", &digest("n-live-9")),
                StatusAnswer::Unknown
            );
            let _live = freeze_copy(std::slice::from_ref(&handle));
            stop_quietly(&handle, "followed source").await;
        }

        /// Guard-retention regression (commit-wins) on the worker-side
        /// publication hook: the entire sorted guard vector stays alive
        /// across the copy and the settle, and the hook observes the actual
        /// pre-write attempt behind these exact guards. `WouldBlock`
        /// discriminates retention — dropped guards would read `Acquired`.
        /// The hook is released BEFORE settlement so the writer's real write
        /// attempt genuinely contends with the retained guards; settling
        /// while parked would hide dropped guards. HWM waits run guard-free.
        #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn retained_guards_block_observed_publish_attempt() {
            let root = tempfile::tempdir().expect("capture root");
            let manager =
                SourceManager::new(root.path().join("capture"), small_config()).expect("manager");
            let live_path = root.path().join("live.log");
            let stable_path = root.path().join("stable.log");
            std::fs::write(&live_path, "r-0\nr-1\nr-2\n").expect("fixture");
            std::fs::write(&stable_path, "s-1\ns-2\ns-3\n").expect("fixture");
            let live = manager
                .start(file_definition(
                    SourceId(uuid::Uuid::from_u128(0xC004)),
                    &live_path,
                    true,
                ))
                .await
                .expect("start followed source");
            let stable = manager
                .start(file_definition(
                    SourceId(uuid::Uuid::from_u128(0xC005)),
                    &stable_path,
                    false,
                ))
                .await
                .expect("start stable source");
            wait_for_records(&live, 3).await;
            wait_for_records(&stable, 3).await;

            let table = CommitTable::new("worker-session-3".to_owned());
            let handles = [live.clone(), stable.clone()];
            let frozen = freeze_copy(&handles);
            let request = CommitRequest {
                window_id: "w-3".to_owned(),
                union_view_id: "u-3".to_owned(),
                candidate_generation: 11,
                nonce: "n-retain-11".to_owned(),
                digest: digest("digest-retain-11"),
                frozen: frozen.clone(),
            };
            let CommitAdmission::Verify { attempt_epoch } = table.commit(&request) else {
                panic!("retention commit must be admitted");
            };
            // Armed with guards already held: every attempt from here observes
            // WouldBlock, whether spuriously periodic or the appended line's.
            let outcome = with_pinned(&handles, |current| {
                assert_eq!(current, frozen);
                let mut probe = publish_probe::arm(live.source_id()).expect("arm publish probe");
                {
                    let mut file = std::fs::OpenOptions::new()
                        .append(true)
                        .open(&live_path)
                        .expect("append fixture");
                    use std::io::Write;
                    writeln!(file, "r-3").expect("append line");
                }
                // Exact pre-publication rendezvous: the capture's write
                // attempt behind OUR retained read guards.
                let observation = probe
                    .await_observation(Duration::from_secs(60))
                    .expect("publish attempt observed");
                assert_eq!(
                    observation,
                    PrePublishObservation::WouldBlock,
                    "write attempt must contend behind retained guards (Acquired would name dropped guards)"
                );
                // Release BEFORE settle: the writer proceeds to the real
                // write-lock attempt, which genuinely contends now.
                probe.release();
                table.settle("w-3", "u-3", attempt_epoch, &request, current)
            });
            assert!(
                matches!(outcome, CommitOutcome::Committed { .. }),
                "guard-held settle must name exactly the retained fences: {outcome:?}"
            );
            // Guards are dropped (`with_pinned` returned), so waiting the new
            // high-watermark cannot wedge the writer — and it must arrive,
            // since the released attempt publishes once the guards release.
            wait_for_records(&live, 4).await;
            assert!(refresh_needed(&frozen, &freeze_copy(&handles)));
            stop_quietly(&live, "followed source").await;
            stop_quietly(&stable, "stable source").await;
        }

        /// Inverse: a publish that lands before the guards are taken is
        /// observed by them, so settling the older frozen fence is Stale.
        /// Every wait runs with no guards held.
        #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn publish_before_pin_returns_stale() {
            let root = tempfile::tempdir().expect("capture root");
            let manager =
                SourceManager::new(root.path().join("capture"), small_config()).expect("manager");
            let live_path = root.path().join("live.log");
            let stable_path = root.path().join("stable.log");
            std::fs::write(&live_path, "r-0\nr-1\nr-2\n").expect("fixture");
            std::fs::write(&stable_path, "s-1\ns-2\ns-3\n").expect("fixture");
            let live = manager
                .start(file_definition(
                    SourceId(uuid::Uuid::from_u128(0xC006)),
                    &live_path,
                    true,
                ))
                .await
                .expect("start followed source");
            let stable = manager
                .start(file_definition(
                    SourceId(uuid::Uuid::from_u128(0xC007)),
                    &stable_path,
                    false,
                ))
                .await
                .expect("start stable source");
            wait_for_records(&live, 3).await;
            wait_for_records(&stable, 3).await;

            let handles = [live.clone(), stable.clone()];
            let frozen_old = freeze_copy(&handles);
            // Publish first, with no guards held anywhere.
            {
                let mut file = std::fs::OpenOptions::new()
                    .append(true)
                    .open(&live_path)
                    .expect("append fixture");
                use std::io::Write;
                writeln!(file, "r-3").expect("append line");
            }
            wait_for_records(&live, 4).await;

            let table = CommitTable::new("worker-session-4".to_owned());
            let request = CommitRequest {
                window_id: "w-4".to_owned(),
                union_view_id: "u-4".to_owned(),
                candidate_generation: 12,
                nonce: "n-stale-12".to_owned(),
                digest: digest("digest-stale-12"),
                frozen: frozen_old,
            };
            let CommitAdmission::Verify { attempt_epoch } = table.commit(&request) else {
                panic!("stale commit must be admitted (newer generation)");
            };
            let outcome = with_pinned(&handles, |current| {
                table.settle("w-4", "u-4", attempt_epoch, &request, current)
            });
            match outcome {
                CommitOutcome::Stale { detail } => assert!(
                    detail.contains("advanced during the merge"),
                    "unexpected stale detail: {detail}"
                ),
                other => panic!("publish-first settle must be Stale, got {other:?}"),
            }
            stop_quietly(&live, "followed source").await;
            stop_quietly(&stable, "stable source").await;
        }
    }
}
