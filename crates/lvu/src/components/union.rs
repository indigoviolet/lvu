//! Union dialog draft state: `view1 + view2` as one timestamp-ordered view.
//!
//! Ownership: Muse union-views assignment. This file is new and owned here;
//! shell registration (`components/mod.rs` slot, `LayerId`, palette) is
//! primary-owned and proposed at the bottom of this module (`PROPOSED HOOKS`).
//! Until then it is exercised through `crates/lvu/tests/component_union.rs`,
//! which includes it by path, so no existing component, shell or manifest is
//! touched.
//!
//! The dialog edits a DRAFT (the input list and the union name) while the
//! ACCEPTED union — input view IDs fenced on their accepted revisions — keeps
//! serving rows. Accepting records every input's current accepted revision as
//! the new fence baseline; an input that advances afterwards marks the union
//! stale (refresh, not failure) and live appends re-merge on the new baseline.
//! Rejecting a candidate preserves the entire last-good chain: accepted
//! inputs, revisions and live refresh are untouched and the draft stays
//! editable. Structural validation here (count, duplicates, self-reference)
//! mirrors `lvu-view/src/union.rs`, which remains authoritative at execution
//! time; this layer pre-validates so the dialog can explain a bad spec before
//! submitting it.
//!
//! Deliberately dependency-light (std only): the `Component` implementation,
//! geometry and rendering land with the shell hook, where the dialog-model
//! (§3–§10) and mnemonic (§8.10) audits apply. Nothing here decides row
//! membership or order — that is `lvu-view::union`'s job through Polars.
//!
//! PROPOSED HOOKS (primary-owned; do not apply without assignment):
//!
//! 1. `crates/lvu/src/components/mod.rs`:
//!      `pub mod union;`
//!    plus `use union::UnionDialog;` and a `pub union: UnionDialog` slot on
//!    `Layers` (constructed in `Layers::default`), following the
//!    `CorrelationDialog` precedent.
//! 2. `crates/lvu/src/component.rs`: a `LayerId::Union` variant with dialog
//!    class M (modal task over one view, like Folding/Correlate), palette row
//!    `Union views…` reachable from a derived view, and mnemonic-audited action
//!    labels (`&Add input`, `&Remove`, `&Create union`, `&Cancel`).
//! 3. `crates/lvu/src/app.rs`: union draft/accepted state per derived view
//!    (accepted input IDs + fenced revisions, mirroring `exact_field` as
//!    accepted-state-or-nothing), `take_query_requests` emitting the union
//!    candidate with its fence, and `apply_query_completion` preserving the
//!    prior union on failure. Coordinate with W22 (grouping/folding UI) and
//!    the source-assistance owner (main.rs source methods) — nonoverlapping
//!    regions per the assignment.
//! 4. Persistence (autosave owner): accepted union inputs ride the additive
//!    `presentation_json.union` key shaped in `lvu-view/src/union.rs`
//!    (`StoredUnionShape`); no schema bump.

/// Dialog-side bound mirroring `lvu-view`'s `MAX_UNION_INPUTS`.
///
/// The execution layer re-checks; this bound only keeps the draft list itself
/// bounded so the dialog never offers what submission must refuse.
pub const MAX_UNION_DIALOG_INPUTS: usize = 8;

/// Longest view identity the draft accepts, mirroring the execution bound.
pub const MAX_UNION_DIALOG_VIEW_ID_BYTES: usize = 256;

/// One accepted union input: the view ID plus the accepted revision the union
/// was fenced on when it published.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnionInputRef {
    pub view_id: String,
    pub accepted_revision: u64,
}

/// The accepted union a view is serving: its own ID, the fenced inputs, and
/// the union revision that published them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcceptedUnion {
    pub union_view_id: String,
    pub inputs: Vec<UnionInputRef>,
    pub revision: u64,
}

/// Draft state of the union dialog. Accepted state lives on the view (see
/// module docs); this struct only carries what the dialog edits plus the last
/// rejection, so reopening resumes the edit.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct UnionDialog {
    inputs: Vec<String>,
    name: String,
    error: Option<String>,
    pending_generation: Option<u64>,
}

impl UnionDialog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Draft input IDs in the order the union merges them (ties break by
    /// this order, so it is significant and persisted).
    pub fn inputs(&self) -> &[String] {
        &self.inputs
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn set_name(&mut self, name: impl Into<String>) {
        self.name = name.into();
    }

    /// The last rejection, if any. Kept beside the editable draft: the
    /// accepted union (where one exists) is unaffected.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn pending_generation(&self) -> Option<u64> {
        self.pending_generation
    }

    /// Add one input view to the draft. Rejects over-long IDs, duplicates and
    /// anything past the dialog bound with an actionable message.
    pub fn add_input(&mut self, view_id: impl Into<String>) -> Result<(), String> {
        let view_id = view_id.into();
        if view_id.is_empty() || view_id.len() > MAX_UNION_DIALOG_VIEW_ID_BYTES {
            return Err(format!(
                "view identity must be 1..={} bytes",
                MAX_UNION_DIALOG_VIEW_ID_BYTES
            ));
        }
        if self.inputs.contains(&view_id) {
            return Err(format!("'{view_id}' is already an input of this union"));
        }
        if self.inputs.len() >= MAX_UNION_DIALOG_INPUTS {
            return Err(format!(
                "a union takes at most {MAX_UNION_DIALOG_INPUTS} input views"
            ));
        }
        self.inputs.push(view_id);
        self.error = None;
        Ok(())
    }

    /// Remove one draft input. Removing the last input is allowed in the
    /// draft; creation still requires at least two (see `validate_for_create`).
    pub fn remove_input(&mut self, view_id: &str) -> bool {
        let before = self.inputs.len();
        self.inputs.retain(|id| id != view_id);
        let removed = self.inputs.len() != before;
        if removed {
            self.error = None;
        }
        removed
    }

    /// Structural validation before submission: at least two inputs, within
    /// the bound, no duplicates, and never the union view itself.
    pub fn validate_for_create(&self, union_view_id: &str) -> Result<(), String> {
        if self.inputs.len() < 2 {
            return Err("a union needs at least two input views".into());
        }
        if self.inputs.len() > MAX_UNION_DIALOG_INPUTS {
            return Err(format!(
                "a union takes at most {MAX_UNION_DIALOG_INPUTS} input views"
            ));
        }
        validate_union_ids(union_view_id, &self.inputs)
    }

    /// Transitive cycle check against the stored dependency graph.
    ///
    /// `resolve` maps a union view ID to its own input IDs (`None` for
    /// ordinary views). Bounded like the execution check; corrupt graphs
    /// report a cycle rather than hanging the dialog.
    pub fn validate_no_cycle(
        &self,
        union_view_id: &str,
        resolve: impl Fn(&str) -> Option<Vec<String>>,
    ) -> Result<(), String> {
        const MAX_VISITED: usize = 64;
        let mut visited = std::collections::HashSet::new();
        let mut work: Vec<String> = self.inputs.clone();
        while let Some(next) = work.pop() {
            if next == union_view_id {
                return Err(format!("union inputs reach back to '{union_view_id}'"));
            }
            if !visited.insert(next.clone()) {
                continue;
            }
            if visited.len() > MAX_VISITED {
                return Err(format!(
                    "dependency graph exceeds {MAX_VISITED} views; refusing the union"
                ));
            }
            if let Some(inputs) = resolve(&next) {
                work.extend(inputs.into_iter().take(MAX_UNION_DIALOG_INPUTS + 1));
            }
        }
        Ok(())
    }

    /// Accept the draft into a fenced union description.
    ///
    /// `current_revision` reports each input's CURRENT accepted revision
    /// (`None` for an unknown/closed view). The returned value records those
    /// revisions as the fence baseline the execution layer publishes under;
    /// any input that advances afterwards is a refresh, and any input that
    /// vanished is a rejection that preserves the prior union.
    pub fn accept(
        &mut self,
        union_view_id: &str,
        revision: u64,
        current_revision: impl Fn(&str) -> Option<u64>,
    ) -> Result<AcceptedUnion, String> {
        self.validate_for_create(union_view_id)?;
        let mut inputs = Vec::with_capacity(self.inputs.len());
        for view_id in &self.inputs {
            match current_revision(view_id) {
                Some(accepted_revision) => inputs.push(UnionInputRef {
                    view_id: view_id.clone(),
                    accepted_revision,
                }),
                None => {
                    let message = format!("input view '{view_id}' is unavailable");
                    self.error = Some(message.clone());
                    return Err(message);
                }
            }
        }
        self.error = None;
        self.pending_generation = None;
        Ok(AcceptedUnion {
            union_view_id: union_view_id.to_owned(),
            inputs,
            revision,
        })
    }

    /// Record a rejection without disturbing accepted state: the draft stays
    /// editable and the caller keeps serving its prior union.
    pub fn reject_candidate(&mut self, message: impl Into<String>) {
        self.error = Some(message.into());
        self.pending_generation = None;
    }

    /// Mark a submission in flight. A second submit while one is pending is a
    /// no-op for the caller to refuse, not a second query.
    pub fn mark_pending(&mut self, generation: u64) -> bool {
        if self.pending_generation.is_some() {
            return false;
        }
        self.pending_generation = Some(generation);
        true
    }

    /// True when any fenced input has advanced past (or vanished from) the
    /// revisions `accepted` published under — i.e. the union wants a refresh.
    pub fn accepted_is_stale(
        accepted: &AcceptedUnion,
        current_revision: impl Fn(&str) -> Option<u64>,
    ) -> bool {
        accepted
            .inputs
            .iter()
            .any(|input| current_revision(&input.view_id) != Some(input.accepted_revision))
    }
}

/// Shared structural rule: no duplicates and never the union view itself.
/// The count floor lives with the caller (`validate_for_create` here,
/// `validate_union_spec` in `lvu-view`), because an empty draft is editable
/// while an empty submission is not.
fn validate_union_ids(union_view_id: &str, inputs: &[String]) -> Result<(), String> {
    let mut seen = std::collections::HashSet::with_capacity(inputs.len());
    for view_id in inputs {
        if view_id == union_view_id {
            return Err(format!("a union cannot contain itself ('{view_id}')"));
        }
        if !seen.insert(view_id) {
            return Err(format!("'{view_id}' is listed twice"));
        }
    }
    Ok(())
}
