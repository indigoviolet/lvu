//! Bounded precise resolution of an accepted enrichment cell for a union.
//!
//! The UI captures [`SharedKeyOrigin`] (including the accepted revision and
//! applied generation) before this workflow starts. We freeze exactly that
//! accepted view, replay it with native dtype evidence on a worker thread, and
//! return the sole persisted key DTO, `ExactFieldConstraint`. Registration of
//! the union remains a caller action after [`SharedKeyResolutionPoll::Settled`]
//! returns `Accepted` and the caller has supplied the still-current fence.

use std::collections::BTreeMap;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{Receiver, TryRecvError, sync_channel},
};

use lvu_core::{ExactFieldConstraint, RecordId};
use lvu_query::exact_key_for_record;
use lvu_view::{
    FrozenInput, FrozenInputLimits, FrozenInputRow, FrozenInputSummary, NativeViewAdapter,
    UnionFrozenInput, UnionFrozenRow, UnionLimits, union_frozen_inputs,
};

use crate::shared_key_controller::{SharedKeyCompletion, SharedKeyController, SharedKeyOrigin};

/// A precise lookup is bounded independently of the eventual union merge.
/// The row cap matches the union's accepted-membership cap; the byte cap is
/// deliberately smaller than an export and large enough to inspect a normal
/// accepted view without making origin resolution an unbounded scan.
pub fn shared_key_input_limits() -> FrozenInputLimits {
    FrozenInputLimits {
        batch_records: 512,
        batch_bytes: 1024 * 1024,
        maximum_scanned_records: 1_000_000,
        maximum_input_bytes: 256 * 1024 * 1024,
        maximum_output_records: 1_000_000,
        maximum_output_bytes: 64 * 1024 * 1024,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SharedKeyResolutionPoll {
    Pending,
    Settled(SharedKeyCompletion),
}

type Resolution = Result<(ExactFieldConstraint, String, bool), String>;

/// One in-flight precise origin lookup. Dropping it requests cancellation;
/// the replay observes that flag between bounded batches.
pub struct SharedKeyResolutionJob {
    generation: u64,
    cancel: Arc<AtomicBool>,
    receiver: Receiver<Resolution>,
    settled: bool,
}

impl SharedKeyResolutionJob {
    /// Poll from the app tick. `current_fence` must be read at this tick, not
    /// copied from submission: the controller rejects an origin whose accepted
    /// revision OR applied generation advanced while precise replay ran.
    pub fn poll(
        &mut self,
        controller: &mut SharedKeyController,
        current_fence: Option<(u64, u64)>,
    ) -> SharedKeyResolutionPoll {
        if self.settled {
            return SharedKeyResolutionPoll::Settled(SharedKeyCompletion::Ignored);
        }
        let resolved = match self.receiver.try_recv() {
            Ok(resolved) => resolved,
            Err(TryRecvError::Empty) => return SharedKeyResolutionPoll::Pending,
            Err(TryRecvError::Disconnected) => {
                Err("the accepted-key resolver stopped without a result".into())
            }
        };
        self.settled = true;
        SharedKeyResolutionPoll::Settled(controller.complete(
            self.generation,
            current_fence,
            resolved,
        ))
    }

    pub fn cancel(&mut self, controller: &mut SharedKeyController) {
        self.cancel.store(true, Ordering::Release);
        controller.cancel(self.generation);
        self.settled = true;
    }
}

impl Drop for SharedKeyResolutionJob {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}

/// Start a lookup only after the shell has captured the origin's accepted
/// revision/generation in `origin`. The accepted-output inventory comes from
/// the compiled accepted chain (including named slash/regex captures); this
/// module deliberately does not parse assignment syntax or inspect raw fields.
///
/// A freeze that reports a different fence is rejected synchronously. No
/// caller should register/open a union until the returned job settles with
/// `SharedKeyCompletion::Accepted`.
pub fn begin_shared_key_resolution(
    controller: &mut SharedKeyController,
    adapter: &NativeViewAdapter,
    origin: SharedKeyOrigin,
) -> Result<SharedKeyResolutionJob, SharedKeyCompletion> {
    let frozen = match adapter.freeze_input(&origin.view_id, shared_key_input_limits()) {
        Ok(frozen) => frozen,
        Err(error) => {
            return Err(SharedKeyCompletion::Rejected(format!(
                "could not freeze the accepted origin view: {error}"
            )));
        }
    };
    let generation = begin_from_frozen_summary(controller, origin.clone(), frozen.summary())?;

    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = Arc::clone(&cancel);
    let record_id = origin.record_id;
    let field = origin.field;
    let accepted_revision = origin.accepted_revision;
    let applied_generation = origin.applied_generation;
    let (sender, receiver) = sync_channel(1);
    std::thread::Builder::new()
        .name("lvu-shared-key".into())
        .spawn(move || {
            let result = resolve_frozen_origin(
                frozen,
                &worker_cancel,
                record_id,
                &field,
                accepted_revision,
                applied_generation,
            );
            let _ = sender.send(result);
        })
        .map_err(|error| {
            controller.complete(
                generation,
                Some((origin.accepted_revision, origin.applied_generation)),
                Err(format!(
                    "could not start the accepted-key resolver: {error}"
                )),
            )
        })?;
    Ok(SharedKeyResolutionJob {
        generation,
        cancel,
        receiver,
        settled: false,
    })
}

fn begin_from_frozen_summary(
    controller: &mut SharedKeyController,
    origin: SharedKeyOrigin,
    summary: &FrozenInputSummary,
) -> Result<u64, SharedKeyCompletion> {
    if summary.view_id != origin.view_id
        || (summary.applied_revision, summary.applied_generation)
            != (origin.accepted_revision, origin.applied_generation)
    {
        return Err(SharedKeyCompletion::Stale);
    }
    // Authority comes only from the same fenced accepted snapshot that will
    // be replayed below. A UI/app inventory may be useful for presentation,
    // but can neither add nor remove authority here. Raw views carry none.
    controller
        .begin(origin, &summary.accepted_enrichment_outputs)
        .map_err(SharedKeyCompletion::Rejected)
}

fn resolve_frozen_origin(
    frozen: FrozenInput,
    cancel: &AtomicBool,
    record_id: RecordId,
    field: &str,
    accepted_revision: u64,
    applied_generation: u64,
) -> Resolution {
    let mut selected = None;
    frozen
        .visit_precise(cancel, |batch| {
            for row in batch.rows {
                if row.record.record_id == record_id {
                    observe_origin_row(&mut selected, row, field)?;
                }
            }
            Ok(())
        })
        .map_err(|error| error.to_string())?;
    finish_origin(selected, field, accepted_revision, applied_generation)
}

fn finish_origin(
    selected: Option<FrozenInputRow>,
    field: &str,
    accepted_revision: u64,
    applied_generation: u64,
) -> Resolution {
    let row = selected.ok_or_else(|| {
        "the selected record is no longer available in the accepted origin view".to_owned()
    })?;
    resolve_origin_row(row, field, accepted_revision, applied_generation)
}

fn observe_origin_row(
    selected: &mut Option<FrozenInputRow>,
    row: FrozenInputRow,
    field: &str,
) -> Result<(), String> {
    if selected.is_some() {
        return Err(
            "the selected record appears more than once in the accepted origin view".into(),
        );
    }
    if let Some(reason) = row.omitted_fields.get(field) {
        return Err(format!(
            "accepted enrichment key {field:?} is unavailable for the selected record: {reason}"
        ));
    }
    if !row.fields.contains_key(field) {
        return Err(format!(
            "accepted enrichment key {field:?} is unavailable for the selected record"
        ));
    }
    if !row.field_types.contains_key(field) {
        return Err(format!(
            "accepted enrichment key {field:?} has no native dtype evidence"
        ));
    }
    *selected = Some(row);
    Ok(())
}

/// Decode through the union's existing typed frozen-row contract, then let
/// lvu-query select by stable identity and construct the canonical scalar.
/// This avoids a JSON/value evaluator in the app and ensures origin extraction
/// uses exactly the same dtype authority as the eventual merged predicate.
fn resolve_origin_row(
    row: FrozenInputRow,
    field: &str,
    accepted_revision: u64,
    applied_generation: u64,
) -> Resolution {
    let record_id = row.record.record_id;
    let mut fields = BTreeMap::new();
    fields.insert(
        field.to_owned(),
        row.fields
            .get(field)
            .cloned()
            .ok_or_else(|| format!("accepted enrichment key {field:?} is unavailable"))?,
    );
    let mut field_types = BTreeMap::new();
    field_types.insert(
        field.to_owned(),
        row.field_types
            .get(field)
            .cloned()
            .ok_or_else(|| format!("accepted enrichment key {field:?} has no dtype evidence"))?,
    );
    let input = UnionFrozenInput {
        view_id: "shared-key-origin".into(),
        applied_revision: accepted_revision,
        applied_generation,
        timestamp_column: "capture".into(),
        rows: vec![UnionFrozenRow {
            record_id,
            timestamp_nanos: Some(row.record.captured_at_unix_nanos),
            fields,
            field_types,
            raw: String::new(),
            raw_bytes: Vec::new(),
            captured_at_unix_nanos: row.record.captured_at_unix_nanos,
            stream: row.record.stream,
            acquisition_id: *row.record.acquisition_id.as_bytes(),
            chunk: row.record.chunk,
        }],
    };
    let empty = UnionFrozenInput {
        view_id: "shared-key-empty".into(),
        applied_revision: accepted_revision,
        applied_generation,
        timestamp_column: "capture".into(),
        rows: Vec::new(),
    };
    let frame = union_frozen_inputs(
        "shared-key-resolution",
        &[input, empty],
        &UnionLimits {
            maximum_rows: 1,
            maximum_bytes: 128 * 1024,
        },
    )
    .map_err(|error| format!("could not decode the accepted enrichment key: {error}"))?;
    let (constraint, dtype) =
        exact_key_for_record(&frame, record_id, field).map_err(|error| error.to_string())?;
    Ok((constraint, dtype, true))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use lvu_core::{ChunkPosition, ExactScalar, RawRecord, RecordBytes, SourceId, StreamKind};
    use uuid::Uuid;

    use super::*;

    fn id(sequence: u64) -> RecordId {
        RecordId {
            source_id: SourceId(Uuid::from_u128(7)),
            sequence,
        }
    }

    fn row(sequence: u64, field: Option<(serde_json::Value, &str)>) -> FrozenInputRow {
        let mut fields = BTreeMap::new();
        let mut field_types = BTreeMap::new();
        if let Some((value, dtype)) = field {
            fields.insert("request_key".into(), value);
            field_types.insert("request_key".into(), dtype.into());
        }
        FrozenInputRow {
            record: RawRecord {
                record_id: id(sequence),
                captured_at_unix_nanos: 123,
                stream: StreamKind::File,
                bytes: RecordBytes::default(),
                delimiter: RecordBytes::default(),
                acquisition_id: Uuid::nil(),
                chunk: ChunkPosition::Complete,
            },
            fields,
            field_types,
            raw_field_types: BTreeMap::new(),
            omitted_fields: BTreeMap::new(),
        }
    }

    fn origin(generation: u64) -> SharedKeyOrigin {
        SharedKeyOrigin {
            view_id: "origin".into(),
            accepted_revision: 3,
            applied_generation: generation,
            record_id: id(9),
            field: "request_key".into(),
        }
    }

    #[test]
    fn missing_and_duplicate_origin_fail_closed() {
        let missing = finish_origin(None, "request_key", 3, 5).unwrap_err();
        assert!(missing.contains("no longer available"));

        let mut selected = None;
        observe_origin_row(
            &mut selected,
            row(9, Some((serde_json::json!(42), "UInt64"))),
            "request_key",
        )
        .unwrap();
        let duplicate = observe_origin_row(
            &mut selected,
            row(9, Some((serde_json::json!(42), "UInt64"))),
            "request_key",
        )
        .unwrap_err();
        assert!(duplicate.contains("more than once"));
    }

    #[test]
    fn native_dtype_evidence_flows_from_decoder_into_controller_fence() {
        let resolved = finish_origin(
            Some(row(9, Some((serde_json::json!(42), "UInt64")))),
            "request_key",
            3,
            5,
        )
        .unwrap();
        assert_eq!(resolved.0.value(), &ExactScalar::UnsignedInteger(42));
        assert_eq!(resolved.1, "UInt64");

        let mut controller = SharedKeyController::default();
        let lookup = controller
            .begin(origin(5), &["request_key".into()])
            .unwrap();
        assert_eq!(
            controller.complete(lookup, Some((3, 6)), Ok(resolved)),
            SharedKeyCompletion::Stale
        );
        assert!(controller.last_proven().is_none());
    }

    #[test]
    fn raw_namesake_without_structural_accepted_output_never_starts() {
        let mut controller = SharedKeyController::default();
        // The raw row can contain the same name and a caller/UI can claim it
        // in a hint, but neither is an argument to the authority function.
        let raw_only = row(9, Some((serde_json::json!(42), "UInt64")));
        let forged_ui_hint = ["request_key".to_owned()];
        assert!(raw_only.fields.contains_key(&forged_ui_hint[0]));
        let summary = FrozenInputSummary {
            view_id: "origin".into(),
            applied_revision: 3,
            applied_generation: 5,
            selected_records: None,
            sources: Vec::new(),
            accepted_enrichment_outputs: Vec::new(),
        };
        assert!(matches!(
            begin_from_frozen_summary(&mut controller, origin(5), &summary),
            Err(SharedKeyCompletion::Rejected(_))
        ));
        assert!(controller.pending().is_none());
    }

    #[test]
    fn slash_output_from_same_frozen_summary_has_authority() {
        let mut controller = SharedKeyController::default();
        let summary = FrozenInputSummary {
            view_id: "origin".into(),
            applied_revision: 3,
            applied_generation: 5,
            selected_records: Some(1),
            sources: Vec::new(),
            // The view layer obtains this from compiled membership stages;
            // slash capture syntax and assignments arrive identically here.
            accepted_enrichment_outputs: vec!["request_key".into()],
        };
        assert!(begin_from_frozen_summary(&mut controller, origin(5), &summary).is_ok());
    }
}
