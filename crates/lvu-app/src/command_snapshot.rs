//! Fixed, bounded command inputs and durable publication references.
use crate::command_rows::{CommandResult, CommandResults};
use lvu_command_enrich::EnrichmentEvent;
use lvu_core::{CommandDefinition, RecordId};
use lvu_memory::{CommandAttemptScope, StoredCommandAttempt, WorkspaceStore};
use lvu_view::{
    FrozenInput, FrozenInputError, FrozenInputLimits, FrozenInputStats, FrozenInputSummary,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    sync::atomic::{AtomicBool, Ordering},
};

pub const MAX_RECORDS: usize = 1024;
pub const MAX_INPUT_BYTES: usize = 4 * 1024 * 1024;

pub fn input_limits() -> FrozenInputLimits {
    FrozenInputLimits {
        batch_records: 256,
        batch_bytes: MAX_INPUT_BYTES,
        maximum_scanned_records: 100_000,
        maximum_input_bytes: 256 * 1024 * 1024,
        maximum_output_records: MAX_RECORDS as u64,
        maximum_output_bytes: MAX_INPUT_BYTES as u64,
    }
}

pub struct PreparedCommand {
    pub scope: CommandAttemptScope,
    pub definition: CommandDefinition,
    pub summary: FrozenInputSummary,
    pub stats: FrozenInputStats,
    pub events: Vec<EnrichmentEvent>,
}

pub fn prepare(
    frozen: FrozenInput,
    scope: CommandAttemptScope,
    definition: CommandDefinition,
    cancel: &AtomicBool,
) -> Result<PreparedCommand, String> {
    let summary = frozen.summary().clone();
    let mut events = Vec::new();
    let stats = frozen
        .visit(cancel, |batch| {
            if events.len().saturating_add(batch.rows.len()) > MAX_RECORDS {
                return Err("command input exceeds 1024 records; narrow the view".into());
            }
            events.extend(batch.rows.into_iter().map(|row| EnrichmentEvent {
                raw: String::from_utf8_lossy(&row.record.bytes).into_owned(),
                record: row.record,
                fields: row.fields.into_iter().collect(),
            }));
            Ok(())
        })
        .map_err(|error| match error {
            FrozenInputError::Limited(message) if message.contains("scanned record") || message.contains("input byte") => format!(
                "{message}; source replay is too large. Use a smaller capture or source set; filtering alone may not reduce replay."
            ),
            FrozenInputError::Limited(message) => format!("{message}; narrow the view before running the command"),
            other => other.to_string(),
        })?;
    if cancel.load(Ordering::Acquire) {
        return Err("command preparation cancelled".into());
    }
    if events.is_empty() {
        return Err("the accepted view has no records to run".into());
    }
    Ok(PreparedCommand {
        scope,
        definition,
        summary,
        stats,
        events,
    })
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationReference {
    version: u8,
    pub scope: CommandAttemptScope,
    pub records: Vec<RecordId>,
}

impl PublicationReference {
    pub fn new(scope: CommandAttemptScope, records: Vec<RecordId>) -> Self {
        Self {
            version: 1,
            scope,
            records,
        }
    }

    pub fn encode(&self) -> Result<String, String> {
        let text = serde_json::to_string(self).map_err(|error| error.to_string())?;
        Self::decode(&text, &self.scope.view_id.0.to_string())?;
        Ok(text)
    }

    pub fn decode(text: &str, view: &str) -> Result<Self, String> {
        if text.len() > 128 * 1024 {
            return Err("saved command result reference is too large".into());
        }
        let value: Self = serde_json::from_str(text)
            .map_err(|error| format!("invalid saved command result: {error}"))?;
        if value.version != 1
            || value.scope.view_id.0.to_string() != view
            || value.records.len() > MAX_RECORDS
            || value.records.is_empty()
            || value.records.iter().collect::<HashSet<_>>().len() != value.records.len()
        {
            return Err(
                "saved command result reference has invalid ownership or record bounds".into(),
            );
        }
        Ok(value)
    }

    pub fn restore(
        &self,
        memory: &WorkspaceStore,
        cancel: &AtomicBool,
    ) -> Result<CommandResults, String> {
        if cancel.load(Ordering::Acquire) {
            return Err("command result restoration cancelled".into());
        }
        let records = memory
            .command_attempts(&self.scope, &self.records)
            .map_err(|error| error.to_string())?;
        let mut rows = CommandResults::new();
        for record in records {
            match record.state {
                StoredCommandAttempt::Ready { fields, diagnostic } => {
                    rows.insert(record.record_id, CommandResult { fields, diagnostic });
                }
                _ => {
                    return Err(
                        "saved command result is unavailable; no command was restarted".into(),
                    );
                }
            }
        }
        if cancel.load(Ordering::Acquire) {
            return Err("command result restoration cancelled".into());
        }
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lvu_core::{SourceId, ViewId};
    use lvu_memory::CommandAttemptOutcome;
    use serde_json::json;

    #[test]
    fn durable_reference_restores_typed_results_and_rejects_other_view_or_unavailable_attempts() {
        let root = tempfile::TempDir::new().unwrap();
        let view = ViewId::new();
        let scope = CommandAttemptScope {
            view_id: view,
            stage_id: "terminal".into(),
            command_revision: "command-v1".into(),
            preceding_definition_revision: "prefix-v1".into(),
        };
        let ready = RecordId {
            source_id: SourceId::new(),
            sequence: u64::MAX,
        };
        let pending = RecordId {
            sequence: 0,
            ..ready
        };
        let mut memory = WorkspaceStore::open(root.path()).unwrap();
        assert!(!memory.has_command_attempt(&scope, ready).unwrap());
        let token = memory
            .reserve_command_attempts(&scope, &[ready], 10)
            .unwrap();
        assert!(memory.has_command_attempt(&scope, ready).unwrap());
        let fields = std::collections::BTreeMap::from([
            ("exact".into(), json!(u64::MAX)),
            ("missing".into(), json!(null)),
            ("unicode".into(), json!("界e\u{301}")),
        ]);
        memory
            .complete_command_attempts(
                &token,
                &[(
                    ready,
                    CommandAttemptOutcome::Ready {
                        fields: fields.clone(),
                        diagnostic: Some("reviewed result".into()),
                    },
                )],
            )
            .unwrap();
        memory
            .reserve_command_attempts(&scope, &[pending], 10)
            .unwrap();
        let encoded = PublicationReference::new(scope.clone(), vec![ready])
            .encode()
            .unwrap();
        assert!(PublicationReference::decode(&encoded, &ViewId::new().0.to_string()).is_err());
        assert!(
            PublicationReference::new(scope.clone(), vec![ready, ready])
                .encode()
                .is_err()
        );
        drop(memory);
        let memory = WorkspaceStore::open(root.path()).unwrap();
        let reference = PublicationReference::decode(&encoded, &view.0.to_string()).unwrap();
        let results = reference.restore(&memory, &AtomicBool::new(false)).unwrap();
        assert_eq!(results[&ready].fields, fields);
        assert_eq!(
            results[&ready].diagnostic.as_deref(),
            Some("reviewed result")
        );
        assert!(reference.restore(&memory, &AtomicBool::new(true)).is_err());
        assert!(
            PublicationReference::new(scope, vec![ready, pending])
                .restore(&memory, &AtomicBool::new(false))
                .unwrap_err()
                .contains("unavailable")
        );
    }
}
