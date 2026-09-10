//! Fenced controller for resolving a selected accepted enrichment value.
//!
//! The runtime performs the bounded `FrozenInput::visit_precise` lookup. This
//! controller only owns authority: the origin identity and accepted
//! revision/generation remain attached until a non-null exact constraint with
//! dtype evidence is proven. Failures and stale completions never replace the
//! last proven selection.

use lvu_core::{ExactFieldConstraint, ExactScalar, RecordId};

pub const MAX_EXACT_KEY_DTYPE_BYTES: usize = 128;
pub const MAX_SHARED_KEY_DIAGNOSTIC_BYTES: usize = 1_024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SharedKeyOrigin {
    pub view_id: String,
    pub accepted_revision: u64,
    pub applied_generation: u64,
    pub record_id: RecordId,
    pub field: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProvenSharedKey {
    pub origin: SharedKeyOrigin,
    /// The sole scalar/key DTO. This is what the union persists in its filter.
    pub constraint: ExactFieldConstraint,
    /// Evidence from precise accepted replay; diagnostic/fencing state only.
    pub native_dtype: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SharedKeyCompletion {
    Accepted(ProvenSharedKey),
    Rejected(String),
    Stale,
    Ignored,
}

#[derive(Clone, Debug, Default)]
pub struct SharedKeyController {
    next_generation: u64,
    pending: Option<(u64, SharedKeyOrigin)>,
    last_proven: Option<ProvenSharedKey>,
    error: Option<String>,
}

impl SharedKeyController {
    /// Begin only for an output declared by the origin's accepted enrichment
    /// chain. A same-named raw field is not authority for a removed output.
    pub fn begin(
        &mut self,
        origin: SharedKeyOrigin,
        accepted_enrichment_outputs: &[String],
    ) -> Result<u64, String> {
        if !accepted_enrichment_outputs
            .iter()
            .any(|output| output == &origin.field)
        {
            let message = "select an output of the accepted enrichment chain".to_owned();
            self.pending = None;
            self.error = Some(message.clone());
            return Err(message);
        }
        self.next_generation = self.next_generation.saturating_add(1);
        let generation = self.next_generation;
        self.pending = Some((generation, origin));
        self.error = None;
        Ok(generation)
    }

    pub fn pending(&self) -> Option<(u64, &SharedKeyOrigin)> {
        self.pending
            .as_ref()
            .map(|(generation, origin)| (*generation, origin))
    }

    pub fn last_proven(&self) -> Option<&ProvenSharedKey> {
        self.last_proven.as_ref()
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Settle a precise frozen lookup against the origin's current accepted
    /// fence. `resolved` carries no alternative scalar representation: the
    /// canonical `ExactFieldConstraint` is validated directly.
    pub fn complete(
        &mut self,
        lookup_generation: u64,
        current_fence: Option<(u64, u64)>,
        resolved: Result<(ExactFieldConstraint, String, bool), String>,
    ) -> SharedKeyCompletion {
        let Some((pending_generation, origin)) = self.pending.as_ref() else {
            return SharedKeyCompletion::Ignored;
        };
        if *pending_generation != lookup_generation {
            return SharedKeyCompletion::Ignored;
        }
        let origin = origin.clone();
        self.pending = None;
        if current_fence != Some((origin.accepted_revision, origin.applied_generation)) {
            self.error = Some("the selected view advanced while resolving its key".into());
            return SharedKeyCompletion::Stale;
        }
        let proven = resolved.and_then(|(constraint, native_dtype, derived_ready)| {
            constraint.validate().map_err(|error| error.to_string())?;
            if constraint.field() != origin.field {
                return Err("the resolved key field does not match the selected enrichment".into());
            }
            if !derived_ready {
                return Err("the selected enrichment key is not ready for this record".into());
            }
            if matches!(constraint.value(), ExactScalar::Null) {
                return Err("the selected enrichment key is null".into());
            }
            if native_dtype.is_empty() || native_dtype.len() > MAX_EXACT_KEY_DTYPE_BYTES {
                return Err("the selected enrichment key has invalid native dtype evidence".into());
            }
            Ok(ProvenSharedKey {
                origin,
                constraint,
                native_dtype,
            })
        });
        match proven {
            Ok(proven) => {
                self.error = None;
                self.last_proven = Some(proven.clone());
                SharedKeyCompletion::Accepted(proven)
            }
            Err(message) => {
                let message = bounded(message);
                self.error = Some(message.clone());
                SharedKeyCompletion::Rejected(message)
            }
        }
    }

    /// Cancel only the lookup that owns `lookup_generation`. A stale job may
    /// remain reachable after a newer selection begins; cancelling that old
    /// handle must not erase the newer pending authority.
    pub fn cancel(&mut self, lookup_generation: u64) -> bool {
        if !self
            .pending
            .as_ref()
            .is_some_and(|(generation, _)| *generation == lookup_generation)
        {
            return false;
        }
        self.pending = None;
        self.error = None;
        true
    }
}

fn bounded(mut message: String) -> String {
    if message.len() <= MAX_SHARED_KEY_DIAGNOSTIC_BYTES {
        return message;
    }
    let mut end = MAX_SHARED_KEY_DIAGNOSTIC_BYTES;
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    message.truncate(end);
    message
}
