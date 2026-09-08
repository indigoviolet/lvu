//! Blocking command-enrichment execution for a caller-owned worker thread.

use lvu_command_enrich::{
    AttemptStore, AttemptStoreError, BatchOutcome, CommandEnricher, EnrichmentEvent, EventId,
    Limits, OutcomeState,
};
use lvu_core::{CommandDefinition, RecordId, SourceId};
use lvu_memory::{
    CommandAttemptOutcome, CommandAttemptReservation, CommandAttemptScope, StoredCommandAttempt,
    WorkspaceStore,
};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::atomic::{AtomicBool, Ordering},
};
use thiserror::Error;
use uuid::Uuid;

pub const MAX_COMMAND_EXECUTION_RECORDS: usize = 1024;
pub const MAX_COMMAND_ATTEMPTS_PER_SCOPE: usize = 100_000;
pub const MAX_COMMAND_EXECUTION_INPUT_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_COMMAND_EXECUTION_RESULT_BYTES: usize = 1024 * 1024;
const MAX_RETURNED_DIAGNOSTICS: usize = 128;
const MAX_RETURNED_DIAGNOSTIC_BYTES: usize = 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandExecutionRecord {
    pub record_id: RecordId,
    pub fields: BTreeMap<String, Value>,
    pub diagnostic: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandExecutionResult {
    /// Complete results in the exact order supplied by the caller.
    pub records: Vec<CommandExecutionRecord>,
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Error)]
pub enum CommandExecutionError {
    #[error("command execution requires 1..={MAX_COMMAND_EXECUTION_RECORDS} unique records")]
    InvalidRecords,
    #[error("command execution input exceeds {MAX_COMMAND_EXECUTION_INPUT_BYTES} bytes")]
    InputTooLarge,
    #[error("command execution result exceeds {MAX_COMMAND_EXECUTION_RESULT_BYTES} bytes")]
    ResultTooLarge,
    #[error("command execution was cancelled; reserved records remain attempted")]
    Cancelled,
    #[error("record {record_id:?} was already attempted but has no reusable result: {diagnostic}")]
    AttemptUnavailable {
        record_id: RecordId,
        diagnostic: String,
    },
    #[error("command protocol did not complete: {0}")]
    Protocol(String),
    #[error("command execution store failed: {0}")]
    Store(String),
    #[error("command runner failed: {0}")]
    Runner(String),
}

/// Execute only never-attempted records and return a complete ordered Ready set.
///
/// The caller owns the worker thread and cancellation lifetime. This function
/// performs blocking SQLite and subprocess work and must not run on the UI tick.
pub fn execute_command_enrichment(
    store: &mut WorkspaceStore,
    scope: CommandAttemptScope,
    command: CommandDefinition,
    events: Vec<EnrichmentEvent>,
    cancelled: &AtomicBool,
) -> Result<CommandExecutionResult, CommandExecutionError> {
    execute_command_enrichment_inner(store, scope, command, events, cancelled, || {})
}

fn execute_command_enrichment_inner(
    store: &mut WorkspaceStore,
    scope: CommandAttemptScope,
    command: CommandDefinition,
    events: Vec<EnrichmentEvent>,
    cancelled: &AtomicBool,
    after_final_read: impl FnOnce(),
) -> Result<CommandExecutionResult, CommandExecutionError> {
    if cancelled.load(Ordering::Acquire) {
        return Err(CommandExecutionError::Cancelled);
    }
    if events.is_empty() || events.len() > MAX_COMMAND_EXECUTION_RECORDS {
        return Err(CommandExecutionError::InvalidRecords);
    }
    let mut unique = HashSet::with_capacity(events.len());
    let ids = events
        .iter()
        .map(|event| event.record.record_id)
        .collect::<Vec<_>>();
    if ids.iter().any(|id| !unique.insert(*id)) {
        return Err(CommandExecutionError::InvalidRecords);
    }
    validate_input_size(&events)?;

    let initial = store
        .command_attempts(&scope, &ids)
        .map_err(|error| CommandExecutionError::Store(error.to_string()))?;
    let mut never_attempted = Vec::new();
    for (event, attempt) in events.iter().zip(&initial) {
        if attempt.record_id != event.record.record_id {
            return Err(CommandExecutionError::Store(
                "command attempt lookup returned records out of order".into(),
            ));
        }
        match &attempt.state {
            StoredCommandAttempt::NeverAttempted => never_attempted.push(event.clone()),
            StoredCommandAttempt::Ready { .. } => {}
            StoredCommandAttempt::Reserved => {
                return Err(CommandExecutionError::AttemptUnavailable {
                    record_id: attempt.record_id,
                    diagnostic: "attempt was reserved but no final result is available".into(),
                });
            }
            StoredCommandAttempt::Failed { diagnostic } => {
                return Err(CommandExecutionError::AttemptUnavailable {
                    record_id: attempt.record_id,
                    diagnostic: diagnostic.clone(),
                });
            }
        }
    }

    let mut batch_diagnostics = Vec::new();
    if !never_attempted.is_empty() {
        let mut attempts = DurableAttempts {
            memory: store,
            scope: scope.clone(),
            capacity: MAX_COMMAND_ATTEMPTS_PER_SCOPE,
        };
        let mut runner = CommandEnricher::new(command, 1, Limits::default())
            .map_err(|error| CommandExecutionError::Runner(error.to_string()))?;
        let outcome = runner
            .run_batch_with_store(1, never_attempted, cancelled, &mut attempts)
            .map_err(|error| CommandExecutionError::Runner(error.to_string()))?;
        for diagnostic in &outcome.diagnostics {
            push_bounded_diagnostic(&mut batch_diagnostics, format_diagnostic(diagnostic));
        }
        if !outcome.diagnostics.is_empty() {
            return Err(CommandExecutionError::Protocol(join_diagnostics(
                &batch_diagnostics,
            )));
        }
    }

    let final_attempts = store
        .command_attempts(&scope, &ids)
        .map_err(|error| CommandExecutionError::Store(error.to_string()))?;
    after_final_read();
    if cancelled.load(Ordering::Acquire) {
        return Err(CommandExecutionError::Cancelled);
    }
    let mut records = Vec::with_capacity(events.len());
    for attempt in final_attempts {
        match attempt.state {
            StoredCommandAttempt::Ready { fields, diagnostic } => {
                records.push(CommandExecutionRecord {
                    record_id: attempt.record_id,
                    fields,
                    diagnostic,
                });
            }
            StoredCommandAttempt::Reserved => {
                return Err(CommandExecutionError::AttemptUnavailable {
                    record_id: attempt.record_id,
                    diagnostic: "attempt was reserved but no final result is available".into(),
                });
            }
            StoredCommandAttempt::Failed { diagnostic } => {
                return Err(CommandExecutionError::AttemptUnavailable {
                    record_id: attempt.record_id,
                    diagnostic,
                });
            }
            StoredCommandAttempt::NeverAttempted => {
                return Err(CommandExecutionError::Store(format!(
                    "record {:?} remained unattempted after command execution",
                    attempt.record_id
                )));
            }
        }
    }
    validate_result_size(&records, &batch_diagnostics)?;
    Ok(CommandExecutionResult {
        records,
        diagnostics: batch_diagnostics,
    })
}

struct DurableAttempts<'a> {
    memory: &'a mut WorkspaceStore,
    scope: CommandAttemptScope,
    capacity: usize,
}

fn store_error(error: impl std::fmt::Display) -> AttemptStoreError {
    AttemptStoreError::new(error.to_string())
}

fn record_id(id: &EventId) -> Result<RecordId, AttemptStoreError> {
    Ok(RecordId {
        source_id: SourceId(Uuid::parse_str(&id.source_id).map_err(store_error)?),
        sequence: id.sequence,
    })
}

impl AttemptStore for DurableAttempts<'_> {
    type Reservation = CommandAttemptReservation;

    fn contains(&mut self, id: &EventId) -> Result<bool, AttemptStoreError> {
        self.memory
            .has_command_attempt(&self.scope, record_id(id)?)
            .map_err(store_error)
    }

    fn remaining_capacity(&mut self) -> Result<usize, AttemptStoreError> {
        Ok(self.capacity.saturating_sub(
            self.memory
                .command_attempt_count(&self.scope)
                .map_err(store_error)?,
        ))
    }

    fn reserve(&mut self, ids: &HashSet<EventId>) -> Result<Self::Reservation, AttemptStoreError> {
        let ids = ids.iter().map(record_id).collect::<Result<Vec<_>, _>>()?;
        self.memory
            .reserve_command_attempts(&self.scope, &ids, self.capacity)
            .map_err(store_error)
    }

    fn persist_outcome(
        &mut self,
        token: &Self::Reservation,
        batch: &BatchOutcome,
    ) -> Result<(), AttemptStoreError> {
        let events = batch
            .events
            .iter()
            .map(|event| (event.event.record.record_id, event))
            .collect::<HashMap<_, _>>();
        let mut outcomes = Vec::with_capacity(token.record_ids.len());
        let batch_failed = !batch.diagnostics.is_empty();
        for id in &token.record_ids {
            let event = events
                .get(id)
                .ok_or_else(|| store_error("command result omitted a reserved record"))?;
            let mut diagnostics = Vec::new();
            for diagnostic in batch.diagnostics.iter().chain(&event.diagnostics) {
                push_bounded_diagnostic(&mut diagnostics, format_diagnostic(diagnostic));
            }
            let diagnostic = join_diagnostics(&diagnostics);
            let outcome = if batch_failed {
                CommandAttemptOutcome::Failed {
                    diagnostic: if diagnostic.is_empty() {
                        "command batch failed protocol validation".into()
                    } else {
                        diagnostic
                    },
                }
            } else {
                match event.state {
                    OutcomeState::Ready => CommandAttemptOutcome::Ready {
                        fields: event.derived.clone().into_iter().collect(),
                        diagnostic: (!diagnostic.is_empty()).then_some(diagnostic),
                    },
                    OutcomeState::Error => CommandAttemptOutcome::Failed {
                        diagnostic: if diagnostic.is_empty() {
                            "command produced no valid result".into()
                        } else {
                            diagnostic
                        },
                    },
                }
            };
            outcomes.push((*id, outcome));
        }
        self.memory
            .complete_command_attempts(token, &outcomes)
            .map_err(store_error)
    }
}

fn validate_input_size(events: &[EnrichmentEvent]) -> Result<(), CommandExecutionError> {
    let mut bytes = 0usize;
    for event in events {
        bytes = bytes
            .checked_add(event.raw.len())
            .and_then(|value| value.checked_add(event.record.bytes.len()))
            .and_then(|value| {
                serde_json::to_vec(&event.fields)
                    .ok()
                    .and_then(|fields| value.checked_add(fields.len()))
            })
            .and_then(|value| value.checked_add(256))
            .ok_or(CommandExecutionError::InputTooLarge)?;
        if bytes > MAX_COMMAND_EXECUTION_INPUT_BYTES {
            return Err(CommandExecutionError::InputTooLarge);
        }
    }
    Ok(())
}

fn validate_result_size(
    records: &[CommandExecutionRecord],
    diagnostics: &[String],
) -> Result<(), CommandExecutionError> {
    let mut bytes = diagnostics.iter().try_fold(0usize, |total, diagnostic| {
        total.checked_add(diagnostic.len())
    });
    for record in records {
        bytes = bytes.and_then(|total| {
            serde_json::to_vec(&record.fields)
                .ok()
                .and_then(|fields| total.checked_add(fields.len() + 32))
                .and_then(|total| {
                    total.checked_add(record.diagnostic.as_ref().map_or(0, String::len))
                })
        });
    }
    if bytes.is_none_or(|bytes| bytes > MAX_COMMAND_EXECUTION_RESULT_BYTES) {
        return Err(CommandExecutionError::ResultTooLarge);
    }
    Ok(())
}

fn format_diagnostic(diagnostic: &lvu_command_enrich::Diagnostic) -> String {
    format!("{}: {}", diagnostic.code, diagnostic.message)
}

fn push_bounded_diagnostic(diagnostics: &mut Vec<String>, mut diagnostic: String) {
    if diagnostics.len() >= MAX_RETURNED_DIAGNOSTICS {
        return;
    }
    if diagnostic.len() > MAX_RETURNED_DIAGNOSTIC_BYTES {
        let mut end = MAX_RETURNED_DIAGNOSTIC_BYTES;
        while !diagnostic.is_char_boundary(end) {
            end -= 1;
        }
        diagnostic.truncate(end);
    }
    diagnostics.push(diagnostic);
}

fn join_diagnostics(diagnostics: &[String]) -> String {
    let mut joined = diagnostics.join("\n");
    if joined.len() > lvu_memory::MAX_COMMAND_ATTEMPT_DIAGNOSTIC_BYTES {
        let mut end = lvu_memory::MAX_COMMAND_ATTEMPT_DIAGNOSTIC_BYTES;
        while !joined.is_char_boundary(end) {
            end -= 1;
        }
        joined.truncate(end);
    }
    joined
}

#[cfg(test)]
mod tests {
    use super::*;
    use lvu_core::{ChunkPosition, CommandProgram, RawRecord, RestartPolicy, StreamKind, ViewId};
    use std::{fs, path::Path};
    use tempfile::TempDir;

    fn scope() -> CommandAttemptScope {
        CommandAttemptScope {
            view_id: ViewId::new(),
            stage_id: "stage-1".into(),
            command_revision: "command-1".into(),
            preceding_definition_revision: "prefix-1".into(),
        }
    }

    fn event(source_id: SourceId, sequence: u64) -> EnrichmentEvent {
        EnrichmentEvent {
            record: RawRecord {
                record_id: RecordId {
                    source_id,
                    sequence,
                },
                captured_at_unix_nanos: 0,
                stream: StreamKind::File,
                bytes: format!("raw-{sequence}").into_bytes().into(),
                delimiter: vec![b'\n'].into(),
                acquisition_id: Uuid::nil(),
                chunk: ChunkPosition::Complete,
            },
            raw: format!("raw-{sequence}"),
            fields: serde_json::Map::new(),
        }
    }

    fn helper(temp: &TempDir) -> (CommandDefinition, std::path::PathBuf) {
        let script = temp.path().join("command.py");
        let marker = temp.path().join("delivered");
        fs::write(
            &script,
            r#"import json,sys
marker,mode=sys.argv[1:]
batch=None; rows=[]
for line in sys.stdin:
 request=json.loads(line)
 if request['type']=='batch_begin':
  batch=request; rows=[]
 elif request['type']=='event': rows.append(request)
 elif request['type']=='batch_end':
  with open(marker,'a') as out:
   for row in rows: out.write(str(row['event_id']['sequence'])+'\n')
  for row in rows:
   fields={'value':row['event_id']['sequence']}
   if mode=='large': fields={'blob':'x'*230000}
   if mode=='fail': fields={'raw':'forbidden'}
   print(json.dumps({'type':'event','session':batch['session'],'revision':batch['revision'],'event_id':row['event_id'],'fields':fields}),flush=True)
  if mode!='missing': print(json.dumps({'type':'batch_complete','session':batch['session'],'revision':batch['revision']}),flush=True)
"#,
        )
        .unwrap();
        (
            CommandDefinition {
                program: CommandProgram::Exec {
                    executable: "python".into(),
                    args: vec![
                        script.display().to_string(),
                        marker.display().to_string(),
                        "ok".into(),
                    ],
                },
                cwd: None,
                environment: BTreeMap::new(),
                restart: RestartPolicy::Never,
            },
            marker,
        )
    }

    fn set_mode(command: &mut CommandDefinition, mode: &str) {
        let CommandProgram::Exec { args, .. } = &mut command.program else {
            unreachable!()
        };
        *args.last_mut().unwrap() = mode.into();
    }

    fn seed_ready(
        store: &mut WorkspaceStore,
        scope: &CommandAttemptScope,
        id: RecordId,
        value: u64,
        diagnostic: Option<&str>,
    ) {
        let reservation = store
            .reserve_command_attempts(scope, &[id], MAX_COMMAND_ATTEMPTS_PER_SCOPE)
            .unwrap();
        store
            .complete_command_attempts(
                &reservation,
                &[(
                    (id),
                    CommandAttemptOutcome::Ready {
                        fields: BTreeMap::from([("value".into(), Value::from(value))]),
                        diagnostic: diagnostic.map(str::to_owned),
                    },
                )],
            )
            .unwrap();
    }

    fn marker_lines(path: &Path) -> Vec<String> {
        fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn reuses_ready_without_delivery_and_preserves_order() {
        let temp = TempDir::new().unwrap();
        let mut store = WorkspaceStore::open(temp.path().join("workspace")).unwrap();
        let scope = scope();
        let source = SourceId::new();
        let events = vec![event(source, 2), event(source, 1)];
        seed_ready(
            &mut store,
            &scope,
            events[0].record.record_id,
            2,
            Some("retained detail"),
        );
        seed_ready(&mut store, &scope, events[1].record.record_id, 1, None);
        let (command, marker) = helper(&temp);
        let result =
            execute_command_enrichment(&mut store, scope, command, events, &AtomicBool::new(false))
                .unwrap();
        assert_eq!(
            result
                .records
                .iter()
                .map(|row| row.record_id.sequence)
                .collect::<Vec<_>>(),
            [2, 1]
        );
        assert_eq!(
            result.records[0].diagnostic.as_deref(),
            Some("retained detail")
        );
        assert!(!marker.exists());
    }

    #[test]
    fn mixed_ready_and_new_delivers_only_new_record() {
        let temp = TempDir::new().unwrap();
        let mut store = WorkspaceStore::open(temp.path().join("workspace")).unwrap();
        let scope = scope();
        let source = SourceId::new();
        let events = vec![event(source, 1), event(source, 2)];
        seed_ready(&mut store, &scope, events[0].record.record_id, 1, None);
        let (command, marker) = helper(&temp);
        let result =
            execute_command_enrichment(&mut store, scope, command, events, &AtomicBool::new(false))
                .unwrap();
        assert_eq!(marker_lines(&marker), ["2"]);
        assert_eq!(
            result
                .records
                .iter()
                .map(|row| row.record_id.sequence)
                .collect::<Vec<_>>(),
            [1, 2]
        );
    }

    #[test]
    fn unavailable_attempt_fails_before_any_new_delivery() {
        let temp = TempDir::new().unwrap();
        let mut store = WorkspaceStore::open(temp.path().join("workspace")).unwrap();
        let scope = scope();
        let source = SourceId::new();
        let reserved = event(source, 1);
        store
            .reserve_command_attempts(
                &scope,
                &[reserved.record.record_id],
                MAX_COMMAND_ATTEMPTS_PER_SCOPE,
            )
            .unwrap();
        let (command, marker) = helper(&temp);
        let error = execute_command_enrichment(
            &mut store,
            scope,
            command,
            vec![reserved, event(source, 2)],
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            CommandExecutionError::AttemptUnavailable { .. }
        ));
        assert!(!marker.exists());
    }

    #[test]
    fn failed_new_record_keeps_prior_ready_immutable() {
        let temp = TempDir::new().unwrap();
        let mut store = WorkspaceStore::open(temp.path().join("workspace")).unwrap();
        let scope = scope();
        let source = SourceId::new();
        let old = event(source, 1);
        let new = event(source, 2);
        seed_ready(&mut store, &scope, old.record.record_id, 11, None);
        let (mut command, marker) = helper(&temp);
        set_mode(&mut command, "fail");
        let error = execute_command_enrichment(
            &mut store,
            scope.clone(),
            command,
            vec![old.clone(), new.clone()],
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(
            matches!(error, CommandExecutionError::AttemptUnavailable { record_id, .. } if record_id == new.record.record_id)
        );
        assert_eq!(marker_lines(&marker), ["2"]);
        let stored = store
            .command_attempts(&scope, &[old.record.record_id, new.record.record_id])
            .unwrap();
        assert!(
            matches!(&stored[0].state, StoredCommandAttempt::Ready { fields, .. } if fields["value"] == 11)
        );
        assert!(matches!(
            &stored[1].state,
            StoredCommandAttempt::Failed { .. }
        ));
    }

    #[test]
    fn missing_completion_cannot_become_success_through_ready_reuse() {
        let temp = TempDir::new().unwrap();
        let mut store = WorkspaceStore::open(temp.path().join("workspace")).unwrap();
        let scope = scope();
        let source = SourceId::new();
        let events = vec![event(source, 1), event(source, 2)];
        let (mut command, marker) = helper(&temp);
        set_mode(&mut command, "missing");
        let first = execute_command_enrichment(
            &mut store,
            scope.clone(),
            command.clone(),
            events.clone(),
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(matches!(first, CommandExecutionError::Protocol(_)));
        assert_eq!(marker_lines(&marker), ["1", "2"]);
        let ids = events
            .iter()
            .map(|event| event.record.record_id)
            .collect::<Vec<_>>();
        let stored = store.command_attempts(&scope, &ids).unwrap();
        assert!(stored.iter().all(|attempt| matches!(
            &attempt.state,
            StoredCommandAttempt::Failed { diagnostic }
                if diagnostic.contains("missing_completion")
        )));

        let second =
            execute_command_enrichment(&mut store, scope, command, events, &AtomicBool::new(false))
                .unwrap_err();
        assert!(matches!(
            second,
            CommandExecutionError::AttemptUnavailable { .. }
        ));
        assert_eq!(marker_lines(&marker), ["1", "2"]);
    }

    #[test]
    fn bounds_and_precancel_reject_without_delivery() {
        let temp = TempDir::new().unwrap();
        let mut store = WorkspaceStore::open(temp.path().join("workspace")).unwrap();
        let (command, marker) = helper(&temp);
        let source = SourceId::new();
        assert!(matches!(
            execute_command_enrichment(
                &mut store,
                scope(),
                command.clone(),
                Vec::new(),
                &AtomicBool::new(false),
            ),
            Err(CommandExecutionError::InvalidRecords)
        ));
        let cancelled = AtomicBool::new(true);
        assert!(matches!(
            execute_command_enrichment(
                &mut store,
                scope(),
                command.clone(),
                vec![event(source, 1)],
                &cancelled,
            ),
            Err(CommandExecutionError::Cancelled)
        ));
        let too_many = (0..=MAX_COMMAND_EXECUTION_RECORDS)
            .map(|sequence| event(source, sequence as u64))
            .collect();
        assert!(matches!(
            execute_command_enrichment(
                &mut store,
                scope(),
                command,
                too_many,
                &AtomicBool::new(false),
            ),
            Err(CommandExecutionError::InvalidRecords)
        ));
        assert!(!marker.exists());
    }

    #[test]
    fn input_and_durable_result_byte_limits_are_not_sampled() {
        let temp = TempDir::new().unwrap();
        let mut store = WorkspaceStore::open(temp.path().join("workspace")).unwrap();
        let source = SourceId::new();
        let (mut command, marker) = helper(&temp);
        let mut oversized = event(source, 1);
        oversized.raw = "x".repeat(MAX_COMMAND_EXECUTION_INPUT_BYTES);
        assert!(matches!(
            execute_command_enrichment(
                &mut store,
                scope(),
                command.clone(),
                vec![oversized],
                &AtomicBool::new(false),
            ),
            Err(CommandExecutionError::InputTooLarge)
        ));
        assert!(!marker.exists());

        set_mode(&mut command, "large");
        let events = (2..7).map(|sequence| event(source, sequence)).collect();
        let error = execute_command_enrichment(
            &mut store,
            scope(),
            command,
            events,
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(matches!(error, CommandExecutionError::Runner(_)));
        assert_eq!(marker_lines(&marker), ["2", "3", "4", "5", "6"]);

        let diagnostic_records = (0..65)
            .map(|sequence| CommandExecutionRecord {
                record_id: RecordId {
                    source_id: source,
                    sequence,
                },
                fields: BTreeMap::new(),
                diagnostic: Some("d".repeat(lvu_memory::MAX_COMMAND_ATTEMPT_DIAGNOSTIC_BYTES)),
            })
            .collect::<Vec<_>>();
        assert!(matches!(
            validate_result_size(&diagnostic_records, &[]),
            Err(CommandExecutionError::ResultTooLarge)
        ));
    }

    #[test]
    fn cancellation_after_all_reused_final_read_returns_cancelled() {
        let temp = TempDir::new().unwrap();
        let mut store = WorkspaceStore::open(temp.path().join("workspace")).unwrap();
        let scope = scope();
        let source = SourceId::new();
        let events = vec![event(source, 1)];
        seed_ready(&mut store, &scope, events[0].record.record_id, 1, None);
        let (command, marker) = helper(&temp);
        let cancelled = AtomicBool::new(false);
        let error = execute_command_enrichment_inner(
            &mut store,
            scope,
            command,
            events,
            &cancelled,
            || cancelled.store(true, Ordering::Release),
        )
        .unwrap_err();
        assert!(matches!(error, CommandExecutionError::Cancelled));
        assert!(!marker.exists());
    }

    #[test]
    fn successful_results_survive_real_sqlite_reopen() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("workspace");
        let scope = scope();
        let source = SourceId::new();
        let events = vec![event(source, 1)];
        let (command, marker) = helper(&temp);
        {
            let mut store = WorkspaceStore::open(&root).unwrap();
            execute_command_enrichment(
                &mut store,
                scope.clone(),
                command.clone(),
                events.clone(),
                &AtomicBool::new(false),
            )
            .unwrap();
        }
        fs::remove_file(&marker).unwrap();
        let mut reopened = WorkspaceStore::open(root).unwrap();
        let result = execute_command_enrichment(
            &mut reopened,
            scope,
            command,
            events,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(result.records[0].fields["value"], Value::from(1));
        assert!(!marker.exists());
    }
}
