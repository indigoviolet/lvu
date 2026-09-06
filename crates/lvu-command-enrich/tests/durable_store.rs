//! The actual SQLite store and subprocess runner, before app/UI integration.
use lvu_command_enrich::*;
use lvu_core::{
    ChunkPosition, CommandDefinition, CommandProgram, RawRecord, RecordId, RestartPolicy, SourceId,
    StreamKind, ViewId,
};
use lvu_memory::{
    CommandAttemptOutcome, CommandAttemptReservation, CommandAttemptScope, StoredCommandAttempt,
    WorkspaceStore,
};
use serde_json::{Map, Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    path::Path,
    sync::atomic::AtomicBool,
    time::Duration,
};
use tempfile::TempDir;
use uuid::Uuid;

struct SqliteAttempts {
    db: WorkspaceStore,
    scope: CommandAttemptScope,
    lose_reservation_ack: bool,
    fail_finalization: bool,
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

impl AttemptStore for SqliteAttempts {
    type Reservation = CommandAttemptReservation;

    fn contains(&mut self, id: &EventId) -> Result<bool, AttemptStoreError> {
        let records = self
            .db
            .command_attempts(&self.scope, &[record_id(id)?])
            .map_err(store_error)?;
        Ok(!matches!(
            records[0].state,
            StoredCommandAttempt::NeverAttempted
        ))
    }

    fn remaining_capacity(&mut self) -> Result<usize, AttemptStoreError> {
        Ok(4usize.saturating_sub(
            self.db
                .command_attempt_count(&self.scope)
                .map_err(store_error)?,
        ))
    }

    fn reserve(&mut self, ids: &HashSet<EventId>) -> Result<Self::Reservation, AttemptStoreError> {
        let ids = ids.iter().map(record_id).collect::<Result<Vec<_>, _>>()?;
        let token = self
            .db
            .reserve_command_attempts(&self.scope, &ids, 4)
            .map_err(store_error)?;
        if self.lose_reservation_ack {
            return Err(store_error(
                "injected lost acknowledgement after SQLite commit",
            ));
        }
        Ok(token)
    }

    fn persist_outcome(
        &mut self,
        token: &Self::Reservation,
        batch: &BatchOutcome,
    ) -> Result<(), AttemptStoreError> {
        if self.fail_finalization {
            return Err(store_error("injected persistence failure after delivery"));
        }
        let events = batch
            .events
            .iter()
            .map(|event| (event.event.record.record_id, event))
            .collect::<HashMap<_, _>>();
        let outcomes = token
            .record_ids
            .iter()
            .map(|id| {
                let event = events[id];
                let diagnostic = batch
                    .diagnostics
                    .iter()
                    .chain(&event.diagnostics)
                    .map(|item| format!("{}: {}", item.code, item.message))
                    .collect::<Vec<_>>()
                    .join("\n");
                let outcome = match event.state {
                    OutcomeState::Ready => CommandAttemptOutcome::Ready {
                        fields: event.derived.clone().into_iter().collect(),
                        diagnostic: (!diagnostic.is_empty()).then_some(diagnostic),
                    },
                    OutcomeState::Error => CommandAttemptOutcome::Failed { diagnostic },
                };
                (*id, outcome)
            })
            .collect::<Vec<_>>();
        self.db
            .complete_command_attempts(token, &outcomes)
            .map_err(store_error)
    }
}

fn scope() -> CommandAttemptScope {
    CommandAttemptScope {
        view_id: ViewId::new(),
        stage_id: "command-stage".into(),
        command_revision: "command-definition-1".into(),
        preceding_definition_revision: "native-chain-1".into(),
    }
}

fn open(root: &Path, scope: &CommandAttemptScope) -> SqliteAttempts {
    SqliteAttempts {
        db: WorkspaceStore::open(root).unwrap(),
        scope: scope.clone(),
        lose_reservation_ack: false,
        fail_finalization: false,
    }
}

fn event(source: SourceId, sequence: u64) -> EnrichmentEvent {
    EnrichmentEvent {
        record: RawRecord {
            record_id: RecordId {
                source_id: source,
                sequence,
            },
            captured_at_unix_nanos: 0,
            stream: StreamKind::File,
            bytes: vec![0xff, 0, 13],
            delimiter: vec![10],
            acquisition_id: Uuid::nil(),
            chunk: ChunkPosition::Complete,
        },
        raw: format!("record {sequence}"),
        fields: Map::from_iter([("prior".into(), json!([null, true, "東京"]))]),
    }
}

fn runner(root: &Path, mode: &str) -> CommandEnricher {
    let helper = root.join("command.py");
    fs::write(&helper, r#"import json,sys
marker,mode=sys.argv[1:]
batch=None; rows=[]
for line in sys.stdin:
 r=json.loads(line)
 if r['type']=='batch_begin': batch=r; rows=[]
 elif r['type']=='event':
  rows.append(r)
  with open(marker,'a') as f: f.write(json.dumps(r['event_id'])+'\n')
 elif r['type']=='batch_end':
  for row in reversed(rows):
   result={'type':'event','session':batch['session'],'revision':batch['revision'],'event_id':row['event_id'],'fields':{'value':row['event_id']['sequence'],'prior_copy':row['fields']['prior']}}
   print(json.dumps(result),flush=True)
   if mode=='duplicate': print(json.dumps(result),flush=True)
  print(json.dumps({'type':'batch_complete','session':batch['session'],'revision':batch['revision']}),flush=True)
"#).unwrap();
    CommandEnricher::new(
        CommandDefinition {
            program: CommandProgram::Exec {
                executable: "python".into(),
                args: vec![
                    helper.display().to_string(),
                    root.join("delivered").display().to_string(),
                    mode.into(),
                ],
            },
            cwd: None,
            environment: BTreeMap::new(),
            restart: RestartPolicy::Never,
        },
        1,
        Limits {
            timeout: Duration::from_secs(3),
            ..Limits::default()
        },
    )
    .unwrap()
}

fn delivered(root: &Path) -> Vec<u64> {
    fs::read_to_string(root.join("delivered"))
        .unwrap_or_default()
        .lines()
        .map(|line| {
            serde_json::from_str::<Value>(line).unwrap()["sequence"]
                .as_u64()
                .unwrap()
        })
        .collect()
}

#[test]
fn sqlite_reservation_survives_lost_ack_and_prevents_delivery_after_reopen() {
    let temp = TempDir::new().unwrap();
    let scope = scope();
    let source = SourceId::new();
    let mut store = open(temp.path(), &scope);
    store.lose_reservation_ack = true;
    let mut command = runner(temp.path(), "valid");
    assert!(matches!(
        command.run_batch_with_store(
            1,
            vec![event(source, 1)],
            &AtomicBool::new(false),
            &mut store
        ),
        Err(RunnerError::AttemptStore(_))
    ));
    assert!(delivered(temp.path()).is_empty());
    drop(command);
    drop(store);
    let mut reopened = open(temp.path(), &scope);
    let result = runner(temp.path(), "valid")
        .run_batch_with_store(
            1,
            vec![event(source, 1)],
            &AtomicBool::new(false),
            &mut reopened,
        )
        .unwrap();
    assert!(
        result.events[0]
            .diagnostics
            .iter()
            .any(|d| d.code == "already_attempted")
    );
    assert!(matches!(
        reopened
            .db
            .command_attempts(&scope, &[event(source, 1).record.record_id])
            .unwrap()[0]
            .state,
        StoredCommandAttempt::Reserved
    ));
    assert!(delivered(temp.path()).is_empty());
}

#[test]
fn sqlite_ready_results_survive_mixed_batches_and_reordered_native_protocol_output() {
    let temp = TempDir::new().unwrap();
    let scope = scope();
    let source = SourceId::new();
    let mut store = open(temp.path(), &scope);
    let mut command = runner(temp.path(), "valid");
    let outcome = command
        .run_batch_with_store(
            1,
            vec![event(source, u64::MAX), event(source, 1)],
            &AtomicBool::new(false),
            &mut store,
        )
        .unwrap();
    assert_eq!(outcome.events[0].state, OutcomeState::Ready, "{outcome:?}");
    assert_eq!(outcome.events[0].event.record.bytes, [0xff, 0, 13]);
    assert_eq!(outcome.events[0].derived["value"], json!(u64::MAX));
    drop(command);
    drop(store);
    let mut reopened = open(temp.path(), &scope);
    runner(temp.path(), "valid")
        .run_batch_with_store(
            1,
            vec![event(source, 1), event(source, 2)],
            &AtomicBool::new(false),
            &mut reopened,
        )
        .unwrap();
    let ids = [
        event(source, u64::MAX).record.record_id,
        event(source, 1).record.record_id,
        event(source, 2).record.record_id,
    ];
    let stored = reopened.db.command_attempts(&scope, &ids).unwrap();
    assert_eq!(
        stored.iter().map(|row| row.record_id).collect::<Vec<_>>(),
        ids
    );
    for (row, sequence) in stored.iter().zip([u64::MAX, 1, 2]) {
        let StoredCommandAttempt::Ready { fields, .. } = &row.state else {
            panic!("expected Ready: {:?}", row.state)
        };
        assert_eq!(fields["value"], json!(sequence));
        assert_eq!(fields["prior_copy"], json!([null, true, "東京"]));
    }
    assert_eq!(delivered(temp.path()), [u64::MAX, 1, 2]);
}

#[test]
fn sqlite_keeps_attempt_after_delivered_result_cannot_be_finalized() {
    let temp = TempDir::new().unwrap();
    let scope = scope();
    let source = SourceId::new();
    let mut store = open(temp.path(), &scope);
    store.fail_finalization = true;
    let mut command = runner(temp.path(), "valid");
    assert!(matches!(
        command.run_batch_with_store(
            1,
            vec![event(source, 1)],
            &AtomicBool::new(false),
            &mut store
        ),
        Err(RunnerError::AttemptStore(_))
    ));
    drop(command);
    drop(store);
    let mut reopened = open(temp.path(), &scope);
    runner(temp.path(), "valid")
        .run_batch_with_store(
            1,
            vec![event(source, 1)],
            &AtomicBool::new(false),
            &mut reopened,
        )
        .unwrap();
    assert_eq!(delivered(temp.path()), [1]);
    assert!(matches!(
        reopened
            .db
            .command_attempts(&scope, &[event(source, 1).record.record_id])
            .unwrap()[0]
            .state,
        StoredCommandAttempt::Reserved
    ));
}
