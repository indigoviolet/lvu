//! Per-window save bases: which version the next save carries as its
//! compare-and-swap base. Mirrors the local worker thread's `versions` map
//! (`lvu-app/src/memory.rs`) exactly — same updates, same non-updates — so
//! shared saves keep local conflict semantics instead of inventing new ones.
//!
//! The rules, each enforced by a test below:
//!
//! - A view never saved nor loaded this session has no base (`None`): the
//!   store inserts it at version 0, and the same `None` against an existing
//!   row conflicts permanently (verified store semantics: `INSERT` of an
//!   existing id never matches). There is no silent first-write-wins.
//! - A successful save records the worker-echoed version as the next base.
//! - A failed save changes NOTHING. In particular a conflict keeps the
//!   last-success base: adopting the peer's echoed version would let the
//!   next automatic save (bookmark, selection, follow change) commit our
//!   stale full draft on top of the peer — a silent lost update. A
//!   conflicted view keeps failing loudly until an explicit reconciled
//!   reload reseeds it; there is no recovery by retrying the same state.
//! - A load seeds every returned view's persisted version as its base
//!   (mirroring local load), and a successful derived-view creation seeds
//!   the echoed version (read back from the store post-commit, never
//!   assumed). Seeding is the ONLY way a base changes besides a save:
//!   reload-then-merge is the explicit recovery path, and the merge itself
//!   stays application responsibility at the controller cutover.

use std::collections::HashMap;

use lvu_core::ViewId;

/// Compare-and-swap bases for one window's shared session. Not thread-safe
/// by itself: the owning session drives it sequentially, like every other
/// per-connection client state.
#[derive(Clone, Debug, Default)]
pub struct SaveBases {
    bases: HashMap<ViewId, u64>,
}

impl SaveBases {
    pub fn new() -> Self {
        Self::default()
    }

    /// The base the next save for `view_id` must carry: the last committed
    /// (or seeded) version, or `None` for a view never seen this session.
    pub fn base_for(&self, view_id: ViewId) -> Option<u64> {
        self.bases.get(&view_id).copied()
    }

    /// Record a commit: the worker-echoed version becomes the next base.
    /// Only success moves a base, exactly like the local worker thread.
    pub fn note_saved(&mut self, view_id: ViewId, version: u64) {
        self.bases.insert(view_id, version);
    }

    /// Record a failure: the base is DELIBERATELY unchanged. A conflicted
    /// view keeps its last-success base, so any automatic follow-up save
    /// with the same stale state is refused again instead of overwriting
    /// the peer that just won. Recovery is reload (reseed) plus an explicit
    /// user merge — never a blind retry, never an adopted peer version.
    pub fn note_failed(&mut self, _view_id: ViewId) {
        // Intentionally no map mutation: see the module docs. The parameter
        // names the view the failure belonged to for call-site symmetry
        // with `note_saved`.
    }

    /// Seed bases from a load: every returned view's persisted version
    /// becomes its base, mirroring local load view-for-view. This is the
    /// explicit recovery step: after a conflict, reloading reseeds to
    /// truth, and only then can a merged save converge.
    pub fn seed_from_loaded(&mut self, views: &[lvu_memory::WorkingView]) {
        for view in views {
            self.bases.insert(view.id, view.version);
        }
    }

    /// Seed a freshly created derived view at the worker-echoed version
    /// (read back post-commit by the worker, never assumed here). Without
    /// this the first save after a create would send `None` against an
    /// existing row and conflict permanently.
    pub fn seed_created(&mut self, view_id: ViewId, version: u64) {
        self.bases.insert(view_id, version);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view_id(n: u128) -> ViewId {
        ViewId(uuid::Uuid::from_u128(n))
    }

    fn working_view(id: u128, version: u64) -> lvu_memory::WorkingView {
        lvu_memory::WorkingView {
            id: view_id(id),
            source_id: lvu_core::SourceId(uuid::Uuid::from_u128(1)),
            name: "All events".into(),
            role: lvu_memory::ViewRole::Derived,
            applied_revision_id: None,
            applied_search: String::new(),
            search_draft: None,
            applied_advanced_filter: None,
            advanced_filter_draft: None,
            navigation: lvu_memory::NavigationState {
                selected: None,
                anchor: None,
                follow: true,
            },
            presentation: lvu_memory::PresentationState::default(),
            version,
        }
    }

    #[test]
    fn unseen_view_saves_without_base() {
        let bases = SaveBases::new();
        assert_eq!(bases.base_for(view_id(11)), None);
    }

    #[test]
    fn saved_version_becomes_next_base() {
        let mut bases = SaveBases::new();
        bases.note_saved(view_id(11), 0);
        assert_eq!(bases.base_for(view_id(11)), Some(0));
        bases.note_saved(view_id(11), 1);
        assert_eq!(bases.base_for(view_id(11)), Some(1));
        // Other views are unaffected.
        assert_eq!(bases.base_for(view_id(12)), None);
    }

    #[test]
    fn failed_save_keeps_last_success_base() {
        // HIGH 1: adopting the peer's echoed version here would let the
        // next automatic save overwrite the peer with our stale draft.
        let mut bases = SaveBases::new();
        bases.note_saved(view_id(11), 0);
        bases.note_failed(view_id(11));
        assert_eq!(
            bases.base_for(view_id(11)),
            Some(0),
            "conflict must not move the base: the follow-up save with the same stale state is refused again"
        );
    }

    #[test]
    fn failed_first_save_keeps_no_base() {
        let mut bases = SaveBases::new();
        bases.note_failed(view_id(11));
        assert_eq!(bases.base_for(view_id(11)), None);
    }

    #[test]
    fn load_seeds_every_returned_view() {
        // HIGH 2a: without this the first save after a load sends None
        // against an existing row and conflicts permanently.
        let mut bases = SaveBases::new();
        bases.seed_from_loaded(&[working_view(11, 4), working_view(12, 9)]);
        assert_eq!(bases.base_for(view_id(11)), Some(4));
        assert_eq!(bases.base_for(view_id(12)), Some(9));
    }

    #[test]
    fn load_reseeds_after_conflict_enabling_recovery() {
        // The explicit recovery path: conflict keeps the stale base (loud,
        // safe), then a reconciled reload reseeds to truth and the merged
        // save can converge.
        let mut bases = SaveBases::new();
        bases.note_saved(view_id(11), 0);
        bases.note_failed(view_id(11));
        assert_eq!(bases.base_for(view_id(11)), Some(0));
        bases.seed_from_loaded(&[working_view(11, 1)]);
        assert_eq!(bases.base_for(view_id(11)), Some(1));
    }

    #[test]
    fn created_view_seeds_echoed_version() {
        // HIGH 2b: without this the first save after a create sends None
        // against the just-created row and conflicts permanently.
        let mut bases = SaveBases::new();
        assert_eq!(bases.base_for(view_id(11)), None);
        bases.seed_created(view_id(11), 0);
        assert_eq!(bases.base_for(view_id(11)), Some(0));
    }

    #[test]
    fn bases_are_per_view() {
        let mut bases = SaveBases::new();
        bases.note_saved(view_id(11), 3);
        bases.seed_created(view_id(12), 0);
        bases.note_failed(view_id(11));
        assert_eq!(bases.base_for(view_id(11)), Some(3));
        assert_eq!(bases.base_for(view_id(12)), Some(0));
    }
}
