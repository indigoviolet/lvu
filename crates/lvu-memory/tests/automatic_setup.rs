use lvu_core::{SourceId, ViewId};
use lvu_memory::{
    AUTOMATIC_SETUP_POLICY_VERSION, AUTOMATIC_SETUP_RECEIPT_SCHEMA_VERSION,
    AutomaticSetupFrozenRevision, AutomaticSetupOutcome, AutomaticSetupReceipt,
    MAX_AUTOMATIC_SETUP_DIAGNOSTIC_BYTES, MAX_AUTOMATIC_SETUP_RECEIPTS_PER_SOURCE, MemoryError,
    WorkspaceStore,
};
use rusqlite::{Connection, params};
use tempfile::TempDir;

fn hash(byte: u8) -> String {
    format!("{byte:02x}").repeat(32)
}

fn applied_receipt(source_id: SourceId, recorded_at_unix_nanos: i64) -> AutomaticSetupReceipt {
    AutomaticSetupReceipt {
        schema_version: AUTOMATIC_SETUP_RECEIPT_SCHEMA_VERSION,
        source_id,
        policy_version: AUTOMATIC_SETUP_POLICY_VERSION,
        outcome: AutomaticSetupOutcome::Applied,
        proposal_sha256: Some(hash(0x11)),
        applied_config_sha256: Some(hash(0x22)),
        created_view_id: Some(ViewId::new()),
        frozen: Some(AutomaticSetupFrozenRevision {
            origin_view_id: ViewId::new(),
            persisted_view_version: 7,
            accepted_revision: 11,
            source_generation: 13,
            data_revision: 17,
        }),
        diagnostic: Some("installed Enhanced view".into()),
        recorded_at_unix_nanos,
    }
}

fn failed_receipt(
    source_id: SourceId,
    policy_version: u32,
    recorded_at_unix_nanos: i64,
) -> AutomaticSetupReceipt {
    AutomaticSetupReceipt {
        schema_version: AUTOMATIC_SETUP_RECEIPT_SCHEMA_VERSION,
        source_id,
        policy_version,
        outcome: AutomaticSetupOutcome::Failed,
        proposal_sha256: None,
        applied_config_sha256: None,
        created_view_id: None,
        frozen: None,
        diagnostic: Some("provider unavailable".into()),
        recorded_at_unix_nanos,
    }
}

#[test]
fn receipt_round_trips_across_restart_and_reverts_only_exact_generated_view() {
    let root = TempDir::new().unwrap();
    let source_id = SourceId::new();
    let receipt = applied_receipt(source_id, 100);
    let expected_view = receipt.created_view_id.unwrap();
    let expected_hash = receipt.applied_config_sha256.clone().unwrap();

    WorkspaceStore::open(root.path())
        .unwrap()
        .upsert_automatic_setup_receipt(&receipt)
        .unwrap();
    let reopened = WorkspaceStore::open(root.path()).unwrap();
    assert_eq!(
        reopened
            .get_automatic_setup_receipt(source_id, AUTOMATIC_SETUP_POLICY_VERSION)
            .unwrap(),
        Some(receipt.clone())
    );
    assert_eq!(
        reopened
            .automatic_setup_receipts_for_source(source_id, 4)
            .unwrap(),
        vec![receipt.clone()]
    );
    assert_eq!(
        reopened.list_automatic_setup_receipts(4).unwrap(),
        vec![receipt]
    );

    assert!(matches!(
        reopened.mark_automatic_setup_reverted(
            source_id,
            AUTOMATIC_SETUP_POLICY_VERSION,
            ViewId::new(),
            &expected_hash,
            101,
        ),
        Err(MemoryError::Conflict)
    ));
    assert!(matches!(
        reopened.mark_automatic_setup_reverted(
            source_id,
            AUTOMATIC_SETUP_POLICY_VERSION,
            expected_view,
            &hash(0x33),
            101,
        ),
        Err(MemoryError::Conflict)
    ));
    assert!(
        reopened
            .mark_automatic_setup_reverted(
                source_id,
                AUTOMATIC_SETUP_POLICY_VERSION,
                expected_view,
                &expected_hash,
                102,
            )
            .unwrap()
    );
    assert!(
        !reopened
            .mark_automatic_setup_reverted(
                source_id,
                AUTOMATIC_SETUP_POLICY_VERSION,
                expected_view,
                &expected_hash,
                103,
            )
            .unwrap()
    );
    drop(reopened);

    let reopened = WorkspaceStore::open(root.path()).unwrap();
    let reverted = reopened
        .get_automatic_setup_receipt(source_id, AUTOMATIC_SETUP_POLICY_VERSION)
        .unwrap()
        .unwrap();
    assert_eq!(reverted.outcome, AutomaticSetupOutcome::Reverted);
    assert_eq!(reverted.recorded_at_unix_nanos, 102);
    assert!(
        reopened
            .remove_automatic_setup_receipt(source_id, AUTOMATIC_SETUP_POLICY_VERSION)
            .unwrap()
    );
    assert!(
        !reopened
            .remove_automatic_setup_receipt(source_id, AUTOMATIC_SETUP_POLICY_VERSION)
            .unwrap()
    );
}

#[test]
fn terminal_outcome_replacement_keeps_one_receipt_per_source_and_policy() {
    let root = TempDir::new().unwrap();
    let store = WorkspaceStore::open(root.path()).unwrap();
    let source_id = SourceId::new();
    let first = failed_receipt(source_id, AUTOMATIC_SETUP_POLICY_VERSION, 10);
    store.upsert_automatic_setup_receipt(&first).unwrap();
    let mut replacement = failed_receipt(source_id, AUTOMATIC_SETUP_POLICY_VERSION, 20);
    replacement.outcome = AutomaticSetupOutcome::Unavailable;
    replacement.diagnostic = Some("not configured".into());
    store.upsert_automatic_setup_receipt(&replacement).unwrap();

    assert_eq!(
        store
            .get_automatic_setup_receipt(source_id, AUTOMATIC_SETUP_POLICY_VERSION)
            .unwrap(),
        Some(replacement.clone())
    );
    assert_eq!(
        store.list_automatic_setup_receipts(100).unwrap(),
        vec![replacement]
    );
}

#[test]
fn receipts_and_enumeration_are_bounded_before_writing() {
    let root = TempDir::new().unwrap();
    let store = WorkspaceStore::open(root.path()).unwrap();
    let source_id = SourceId::new();
    for policy in 1..=MAX_AUTOMATIC_SETUP_RECEIPTS_PER_SOURCE as u32 {
        store
            .upsert_automatic_setup_receipt(&failed_receipt(source_id, policy, i64::from(policy)))
            .unwrap();
    }
    let extra_policy = MAX_AUTOMATIC_SETUP_RECEIPTS_PER_SOURCE as u32 + 1;
    assert!(
        store
            .upsert_automatic_setup_receipt(&failed_receipt(source_id, extra_policy, 99))
            .unwrap_err()
            .to_string()
            .contains("at most")
    );
    assert!(
        store
            .automatic_setup_receipts_for_source(
                source_id,
                MAX_AUTOMATIC_SETUP_RECEIPTS_PER_SOURCE as u32 + 1,
            )
            .unwrap_err()
            .to_string()
            .contains("limit")
    );
    assert!(store.list_automatic_setup_receipts(101).is_err());

    let mut oversized = failed_receipt(SourceId::new(), 1, 1);
    oversized.diagnostic = Some("x".repeat(MAX_AUTOMATIC_SETUP_DIAGNOSTIC_BYTES + 1));
    assert!(store.upsert_automatic_setup_receipt(&oversized).is_err());

    let mut unsafe_applied = applied_receipt(SourceId::new(), 1);
    unsafe_applied.applied_config_sha256 = Some("not-a-hash".into());
    assert!(
        store
            .upsert_automatic_setup_receipt(&unsafe_applied)
            .is_err()
    );
    assert_eq!(store.list_automatic_setup_receipts(100).unwrap().len(), 16);
}

#[test]
fn malformed_and_future_receipts_are_reported_without_reset_or_rewrite() {
    let root = TempDir::new().unwrap();
    drop(WorkspaceStore::open(root.path()).unwrap());
    let future_source = SourceId::new();
    let malformed_source = SourceId::new();
    let mut future = failed_receipt(future_source, 1, 42);
    future.schema_version = AUTOMATIC_SETUP_RECEIPT_SCHEMA_VERSION + 1;
    let future_bytes = serde_json::to_vec(&future).unwrap();
    let malformed_bytes = b"{".to_vec();
    let connection = Connection::open(root.path().join("workspace.sqlite3")).unwrap();
    connection
        .execute(
            "INSERT INTO automatic_setup_receipts VALUES(?1,1,?2,42)",
            params![future_source.0.to_string(), &future_bytes],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO automatic_setup_receipts VALUES(?1,1,?2,43)",
            params![malformed_source.0.to_string(), &malformed_bytes],
        )
        .unwrap();
    drop(connection);

    let store = WorkspaceStore::open(root.path()).unwrap();
    assert!(
        store
            .get_automatic_setup_receipt(future_source, 1)
            .unwrap_err()
            .to_string()
            .contains("unsupported automatic setup receipt schema")
    );
    assert!(
        store
            .get_automatic_setup_receipt(malformed_source, 1)
            .is_err()
    );
    drop(store);

    let connection = Connection::open(root.path().join("workspace.sqlite3")).unwrap();
    let still_future: Vec<u8> = connection
        .query_row(
            "SELECT receipt_json FROM automatic_setup_receipts WHERE source_id=?1",
            [future_source.0.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    let still_malformed: Vec<u8> = connection
        .query_row(
            "SELECT receipt_json FROM automatic_setup_receipts WHERE source_id=?1",
            [malformed_source.0.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(still_future, future_bytes);
    assert_eq!(still_malformed, malformed_bytes);
}
