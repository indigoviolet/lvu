//! Observable behaviour of ownership-aware deletion, retention and cache
//! pressure against real temporary capture roots.

#[allow(dead_code)]
#[path = "../src/storage/ledger.rs"]
mod ledger;
#[allow(dead_code)]
#[path = "../src/storage/ownership.rs"]
mod ownership;
#[allow(dead_code)]
#[path = "../src/storage/pressure.rs"]
mod pressure;
#[allow(dead_code)]
#[path = "../src/storage/retention.rs"]
mod retention;

use ledger::{DeletionCause, DeletionLedger, DeletionState, capture_gaps};
use lvu_core::{ChunkPosition, Journal, RawRecord, RecordId, SourceId, StreamKind};
use ownership::{DeletionBlocker, InvestigationRef, OwnershipIndex, ScanRequest, parse_source_id};
use pressure::{
    CacheClass, CacheUsage, DiskSpace, Durability, PressureInputs, PressureLevel,
    assess as assess_pressure,
};
use retention::{RetentionRules, apply as apply_retention, assess as assess_retention};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::Duration,
};
use tempfile::{TempDir, tempdir};

const DAY_NANOS: i64 = 86_400 * 1_000_000_000;

fn root() -> TempDir {
    tempdir().expect("temporary capture root")
}

/// Writes a real journal so sequence and capture-time probes read committed
/// records rather than a hand-made file.
fn capture(root: &Path, name: &str, records: usize, first_captured_at: i64) -> SourceId {
    let source_id = SourceId::new();
    let directory = root.join(source_id.0.to_string());
    fs::create_dir_all(&directory).unwrap();
    fs::write(
        directory.join("source.json"),
        serde_json::json!({
            "schema_version": 1,
            "source_id": source_id.0.to_string(),
            "generation": 3,
            "definition": {"name": name},
        })
        .to_string(),
    )
    .unwrap();
    fs::write(directory.join("events.jsonl"), b"{\"event\":\"running\"}\n").unwrap();
    let (mut journal, _) = Journal::open(directory.join("capture.journal"), source_id).unwrap();
    for index in 0..records {
        journal
            .append(RawRecord {
                record_id: RecordId {
                    source_id,
                    sequence: 0,
                },
                captured_at_unix_nanos: first_captured_at + index as i64,
                stream: StreamKind::File,
                bytes: format!("line {index}\n").into_bytes(),
                delimiter: b"\n".to_vec(),
                acquisition_id: uuid::Uuid::nil(),
                chunk: ChunkPosition::Complete,
            })
            .unwrap();
    }
    journal.sync_data().unwrap();
    drop(journal);
    source_id
}

fn investigation(
    root: &Path,
    name: &str,
    question: &str,
    pins: &[(SourceId, u64, u64)],
) -> PathBuf {
    let directory = root.join("investigations").join(name);
    fs::create_dir_all(&directory).unwrap();
    fs::write(
        directory.join("lvu-investigation.json"),
        serde_json::json!({
            "schema_version": 1,
            "id": format!("investigation-{name}"),
            "created_at": 10_i64,
            "question": question,
        })
        .to_string(),
    )
    .unwrap();
    let sources: Vec<_> = pins
        .iter()
        .map(|(id, generation, watermark)| {
            serde_json::json!({
                "source_id": id.0.to_string(),
                "generation": generation,
                "high_watermark": watermark,
            })
        })
        .collect();
    fs::write(
        directory.join("manifest.json"),
        serde_json::json!({
            "schema_version": 2,
            "investigation_id": format!("investigation-{name}"),
            "sources": sources,
            "source_parts": [],
            "filtered_parts": [],
        })
        .to_string(),
    )
    .unwrap();
    fs::write(directory.join("part-0.parquet"), vec![7_u8; 4096]).unwrap();
    directory
}

fn index_of(root: &Path) -> OwnershipIndex {
    OwnershipIndex::build(
        &ScanRequest {
            root: root.to_path_buf(),
            ..Default::default()
        },
        &AtomicBool::new(false),
    )
}

fn ledger_of(root: &Path) -> DeletionLedger {
    DeletionLedger::new(root)
}

#[test]
fn deleting_a_capture_an_investigation_pins_is_refused_and_removes_nothing() {
    let temp = root();
    let source = capture(temp.path(), "api", 4, 1_000);
    investigation(
        temp.path(),
        "abc",
        "why did the api fail",
        &[(source, 3, 3)],
    );

    let index = index_of(temp.path());
    let plan = index.plan_delete_capture(source, DeletionCause::UserRequested, None);
    assert!(!plan.allowed());
    assert!(matches!(
        plan.blockers.first(),
        Some(DeletionBlocker::PinnedByInvestigation { .. })
    ));
    let summary = plan.summary();
    assert!(summary.contains("why did the api fail"), "{summary}");
    assert!(
        summary.contains("Delete that investigation first"),
        "{summary}"
    );
    // The preview still names the dependency so the user can act on it.
    assert_eq!(plan.dependents.len(), 1);

    let outcome = index.execute(&plan, &ledger_of(temp.path()), &AtomicBool::new(false));
    assert_eq!(outcome.state, DeletionState::Failed);
    assert_eq!(outcome.bytes_freed, 0);
    assert!(
        temp.path()
            .join(source.0.to_string())
            .join("capture.journal")
            .exists()
    );
    // A refusal is not a deletion, so it leaves no boundary behind.
    assert!(!ledger_of(temp.path()).path().exists());
}

#[test]
fn deleting_an_unpinned_capture_records_a_visible_boundary_and_frees_the_bytes() {
    let temp = root();
    let source = capture(temp.path(), "syslog", 6, 5_000);
    let other = capture(temp.path(), "kernel", 2, 6_000);
    investigation(temp.path(), "abc", "kernel panic", &[(other, 3, 1)]);

    let index = index_of(temp.path());
    let plan = index.plan_delete_capture(source, DeletionCause::UserRequested, None);
    assert!(plan.allowed(), "{:?}", plan.blockers);
    assert!(plan.durable_bytes > 0);
    let boundary = plan.boundary.clone().expect("capture boundary");
    assert_eq!(boundary.first_sequence, Some(0));
    assert_eq!(boundary.source_name, "syslog");

    let ledger = ledger_of(temp.path());
    let outcome = index.execute(&plan, &ledger, &AtomicBool::new(false));
    assert_eq!(outcome.state, DeletionState::Completed);
    assert!(outcome.bytes_freed >= boundary.journal_bytes);
    assert!(!temp.path().join(source.0.to_string()).exists());
    assert!(temp.path().join(other.0.to_string()).exists());

    let readout = ledger.read();
    let states: Vec<_> = readout.records.iter().map(|record| record.state).collect();
    assert_eq!(
        states,
        vec![DeletionState::Intended, DeletionState::Completed],
        "intent must be durable before the first unlink"
    );
    let gaps = capture_gaps(&readout);
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].state, DeletionState::Completed);
    assert_eq!(gaps[0].source_id, source.0.to_string());
    assert!(
        gaps[0].summary.contains("records from 0 onward"),
        "{}",
        gaps[0].summary
    );

    // The source is gone, but the gap survives a rescan, so nothing can imply
    // uninterrupted capture over it.
    let rescan = index_of(temp.path());
    assert!(rescan.capture(source).is_none());
    assert_eq!(capture_gaps(&ledger_of(temp.path()).read()).len(), 1);
}

#[test]
fn deleting_the_investigation_releases_its_pin_and_reports_what_it_frees() {
    let temp = root();
    let source = capture(temp.path(), "api", 3, 1_000);
    let directory = investigation(temp.path(), "abc", "why", &[(source, 3, 2)]);
    let reference = InvestigationRef::new("abc").unwrap();

    let index = index_of(temp.path());
    let plan = index.plan_delete_investigation(reference, DeletionCause::UserRequested, None);
    assert!(plan.allowed(), "{:?}", plan.blockers);
    assert!(plan.durable_bytes >= 4096);
    assert_eq!(plan.dependents.len(), 1, "the released pin is reported");
    let outcome = index.execute(&plan, &ledger_of(temp.path()), &AtomicBool::new(false));
    assert_eq!(outcome.state, DeletionState::Completed);
    assert!(!directory.exists());

    let after = index_of(temp.path());
    assert!(
        after
            .plan_delete_capture(source, DeletionCause::UserRequested, None)
            .allowed()
    );
    // Deleting an investigation is recorded, but it is not a capture gap.
    assert!(capture_gaps(&ledger_of(temp.path()).read()).is_empty());
}

#[test]
fn an_unreadable_investigation_manifest_blocks_every_capture_deletion() {
    let temp = root();
    let source = capture(temp.path(), "api", 3, 1_000);
    let directory = investigation(temp.path(), "abc", "why", &[]);
    fs::write(directory.join("manifest.json"), b"{ this is not json").unwrap();

    let index = index_of(temp.path());
    let unit = index
        .investigation(InvestigationRef::new("abc").unwrap())
        .unwrap();
    assert!(unit.unknown_pins);
    let plan = index.plan_delete_capture(source, DeletionCause::UserRequested, None);
    assert!(matches!(
        plan.blockers.first(),
        Some(DeletionBlocker::UnknownPins { .. })
    ));
    assert!(
        plan.summary().contains("Nothing is assumed unreferenced"),
        "{}",
        plan.summary()
    );
}

#[test]
fn an_investigation_without_a_published_dataset_pins_nothing() {
    let temp = root();
    let source = capture(temp.path(), "api", 3, 1_000);
    let directory = investigation(temp.path(), "abc", "why", &[]);
    fs::remove_file(directory.join("manifest.json")).unwrap();

    let index = index_of(temp.path());
    let unit = index
        .investigation(InvestigationRef::new("abc").unwrap())
        .unwrap();
    assert!(!unit.unknown_pins);
    assert!(unit.pins.is_empty());
    assert!(
        index
            .plan_delete_capture(source, DeletionCause::UserRequested, None)
            .allowed()
    );
}

#[test]
fn a_capture_with_a_live_writer_is_refused_until_the_source_stops() {
    use fs2::FileExt;
    let temp = root();
    let source = capture(temp.path(), "follow", 2, 1_000);
    let (journal, _) = Journal::open(
        temp.path()
            .join(source.0.to_string())
            .join("capture.journal"),
        source,
    )
    .unwrap();

    let plan =
        index_of(temp.path()).plan_delete_capture(source, DeletionCause::UserRequested, None);
    assert!(matches!(
        plan.blockers.first(),
        Some(DeletionBlocker::SourceCapturing)
    ));
    drop(journal);

    // The application's own open-source set blocks it independently of the lock.
    let held = OwnershipIndex::build(
        &ScanRequest {
            root: temp.path().to_path_buf(),
            active_sources: BTreeSet::from([source]),
            ..Default::default()
        },
        &AtomicBool::new(false),
    );
    assert!(
        !held
            .plan_delete_capture(source, DeletionCause::UserRequested, None)
            .allowed()
    );

    let released = index_of(temp.path());
    assert!(
        released
            .plan_delete_capture(source, DeletionCause::UserRequested, None)
            .allowed()
    );
    let lock = temp
        .path()
        .join(source.0.to_string())
        .join("capture.journal.lock");
    assert!(fs::File::open(&lock).unwrap().try_lock_exclusive().is_ok());
}

#[test]
fn a_deletion_that_cannot_record_its_boundary_removes_nothing() {
    let temp = root();
    let source = capture(temp.path(), "api", 3, 1_000);
    // The ledger path is occupied by a directory, so the append fails.
    fs::create_dir(temp.path().join("deletions.jsonl")).unwrap();

    let index = index_of(temp.path());
    let plan = index.plan_delete_capture(source, DeletionCause::UserRequested, None);
    assert!(plan.allowed());
    let outcome = index.execute(&plan, &ledger_of(temp.path()), &AtomicBool::new(false));
    assert!(matches!(
        outcome.blockers.first(),
        Some(DeletionBlocker::BoundaryNotRecordable { .. })
    ));
    assert!(
        outcome.detail.contains("nothing was deleted"),
        "{}",
        outcome.detail
    );
    assert!(
        temp.path()
            .join(source.0.to_string())
            .join("capture.journal")
            .exists()
    );
}

#[test]
fn a_stale_preview_cannot_authorize_a_different_deletion() {
    let temp = root();
    let source = capture(temp.path(), "api", 3, 1_000);
    let index = index_of(temp.path());
    let mut plan = index.plan_delete_capture(source, DeletionCause::UserRequested, None);
    plan.digest ^= 1;

    let outcome = index.execute(&plan, &ledger_of(temp.path()), &AtomicBool::new(false));
    assert!(matches!(
        outcome.blockers.first(),
        Some(DeletionBlocker::PlanStale)
    ));
    assert!(temp.path().join(source.0.to_string()).exists());
    assert!(!ledger_of(temp.path()).path().exists());
}

#[test]
fn cancellation_stops_a_scan_and_leaves_an_interrupted_deletion_visible() {
    let temp = root();
    let source = capture(temp.path(), "api", 3, 1_000);

    let cancelled = OwnershipIndex::build(
        &ScanRequest {
            root: temp.path().to_path_buf(),
            ..Default::default()
        },
        &AtomicBool::new(true),
    );
    assert!(cancelled.cancelled);
    assert!(cancelled.captures.is_empty());

    let index = index_of(temp.path());
    let plan = index.plan_delete_capture(source, DeletionCause::UserRequested, None);
    let ledger = ledger_of(temp.path());
    let outcome = index.execute(&plan, &ledger, &AtomicBool::new(true));
    assert_eq!(outcome.state, DeletionState::Failed);
    assert_eq!(outcome.bytes_freed, 0);
    assert!(temp.path().join(source.0.to_string()).exists());
    let gaps = capture_gaps(&ledger.read());
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].state, DeletionState::Failed);
    assert!(
        gaps[0].summary.contains("incomplete"),
        "{}",
        gaps[0].summary
    );
}

#[test]
fn an_interrupted_deletion_without_a_settlement_entry_is_reported_as_interrupted() {
    let temp = root();
    let source = capture(temp.path(), "api", 3, 1_000);
    let index = index_of(temp.path());
    let plan = index.plan_delete_capture(source, DeletionCause::UserRequested, None);
    let ledger = ledger_of(temp.path());
    // Simulate a crash between the recorded intent and the removal.
    let intent = ledger::DeletionRecord::intent(
        ledger::DeletedKind::Capture,
        source.0.to_string(),
        plan.label.clone(),
        DeletionCause::UserRequested,
        None,
        plan.boundary.clone(),
        plan.freed_bytes(),
    );
    ledger.append(&intent).unwrap();

    let gaps = capture_gaps(&ledger.read());
    assert_eq!(gaps[0].state, DeletionState::Intended);
    assert!(
        gaps[0]
            .summary
            .contains("did not observe a completion entry"),
        "{}",
        gaps[0].summary
    );
}

#[cfg(unix)]
#[test]
fn a_permission_failure_leaves_the_capture_and_records_the_partial_removal() {
    use std::os::unix::fs::PermissionsExt;
    let temp = root();
    let source = capture(temp.path(), "api", 3, 1_000);
    let directory = temp.path().join(source.0.to_string());
    let index = index_of(temp.path());
    let plan = index.plan_delete_capture(source, DeletionCause::UserRequested, None);

    fs::set_permissions(&directory, fs::Permissions::from_mode(0o500)).unwrap();
    let ledger = ledger_of(temp.path());
    let outcome = index.execute(&plan, &ledger, &AtomicBool::new(false));
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();

    if outcome.state == DeletionState::Completed {
        // A privileged test runner can unlink regardless of the mode.
        return;
    }
    assert!(!outcome.errors.is_empty());
    assert!(directory.join("capture.journal").exists());
    let gaps = capture_gaps(&ledger.read());
    assert_eq!(gaps[0].state, DeletionState::Failed);
    assert!(
        gaps[0].summary.contains("incomplete"),
        "{}",
        gaps[0].summary
    );
}

#[test]
fn a_symlinked_capture_entry_is_never_followed_out_of_the_capture_root() {
    let temp = root();
    let outside = root();
    let secret = outside.path().join("other.log");
    fs::write(&secret, b"not ours").unwrap();
    let source = SourceId::new();
    #[cfg(unix)]
    std::os::unix::fs::symlink(outside.path(), temp.path().join(source.0.to_string())).unwrap();

    let index = index_of(temp.path());
    assert!(
        index.capture(source).is_none(),
        "a symlink is not a capture"
    );
    let plan = index.plan_delete_capture(source, DeletionCause::UserRequested, None);
    assert!(matches!(
        plan.blockers.first(),
        Some(DeletionBlocker::NotFound)
    ));
    assert!(secret.exists());
}

#[test]
fn retention_is_off_until_a_limit_is_configured() {
    let temp = root();
    capture(temp.path(), "api", 4, 1_000);
    let index = index_of(temp.path());

    let assessment = assess_retention(&index, &RetentionRules::default(), 100 * DAY_NANOS);
    assert!(!assessment.active);
    assert!(assessment.plans.is_empty());
    assert!(
        assessment.summary.contains("Retention is off"),
        "{}",
        assessment.summary
    );

    // Enabling without any limit still selects nothing.
    let enabled = RetentionRules::from_fields(true, None, None, []);
    let assessment = assess_retention(&index, &enabled, 100 * DAY_NANOS);
    assert!(!assessment.active);
    assert!(assessment.plans.is_empty());
}

#[test]
fn retention_by_age_deletes_the_old_capture_records_the_gap_and_protects_the_pinned_one() {
    let temp = root();
    let old = capture(temp.path(), "old", 3, 1_000);
    let pinned = capture(temp.path(), "pinned", 3, 1_000);
    investigation(temp.path(), "abc", "keep me", &[(pinned, 3, 2)]);
    // Age is measured from the last capture activity, which is now.
    let now = ledger::now_unix_nanos() + 30 * DAY_NANOS;

    let index = index_of(temp.path());
    let rules = RetentionRules::from_fields(true, None, Some(Duration::from_secs(7 * 86_400)), []);
    let assessment = assess_retention(&index, &rules, now);
    assert!(assessment.active);
    assert_eq!(assessment.plans.len(), 1);
    assert_eq!(assessment.refused.len(), 1);
    assert!(assessment.unreclaimable_bytes > 0);
    assert!(
        assessment
            .summary
            .contains("are protected and will not be removed"),
        "{}",
        assessment.summary
    );

    let ledger = ledger_of(temp.path());
    let outcomes = apply_retention(&index, &assessment, &ledger, &AtomicBool::new(false));
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].state, DeletionState::Completed);
    assert!(!temp.path().join(old.0.to_string()).exists());
    assert!(temp.path().join(pinned.0.to_string()).exists());

    let gaps = capture_gaps(&ledger.read());
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].cause, DeletionCause::Retention);
    assert!(gaps[0].summary.contains("retention"), "{}", gaps[0].summary);
    assert!(gaps[0].policy.as_deref().unwrap().contains("older than"));
}

#[test]
fn a_global_size_limit_reports_the_shortfall_it_refuses_to_close() {
    let temp = root();
    let pinned = capture(temp.path(), "pinned", 200, 1_000);
    investigation(temp.path(), "abc", "keep me", &[(pinned, 3, 199)]);
    let index = index_of(temp.path());
    let held = index.capture(pinned).unwrap().durable_bytes;

    // A limit far below the protected capture cannot be met by deletion.
    let rules = RetentionRules::from_fields(true, Some(held / 4), None, []);
    let assessment = assess_retention(&index, &rules, ledger::now_unix_nanos());
    assert!(assessment.plans.is_empty());
    assert_eq!(assessment.refused.len(), 1);
    assert!(assessment.shortfall_bytes > 0);
    assert!(
        assessment
            .summary
            .contains("will not delete protected data to reach it"),
        "{}",
        assessment.summary
    );
    let outcomes = apply_retention(
        &index,
        &assessment,
        &ledger_of(temp.path()),
        &AtomicBool::new(false),
    );
    assert!(outcomes.is_empty());
    assert!(temp.path().join(pinned.0.to_string()).exists());
}

#[test]
fn a_per_source_rule_only_matches_its_own_source_by_name_or_identity() {
    let temp = root();
    let small = capture(temp.path(), "small", 2, 1_000);
    let large = capture(temp.path(), "large", 200, 1_000);
    let index = index_of(temp.path());
    let limit = index.capture(small).unwrap().durable_bytes;

    let by_name =
        RetentionRules::from_fields(true, None, None, [("large".to_string(), Some(limit), None)]);
    let assessment = assess_retention(&index, &by_name, ledger::now_unix_nanos());
    assert_eq!(assessment.plans.len(), 1);
    assert!(matches!(
        assessment.plans[0].target,
        ownership::DeletionTarget::Capture(id) if id == large
    ));

    let by_id =
        RetentionRules::from_fields(true, None, None, [(large.0.to_string(), Some(limit), None)]);
    assert_eq!(
        assess_retention(&index, &by_id, ledger::now_unix_nanos())
            .plans
            .len(),
        1
    );
}

#[test]
fn pressure_reclaims_disposable_caches_before_it_ever_stops_acquisition() {
    let reserve = 100_u64;
    let base = PressureInputs {
        reserve_bytes: reserve,
        derived_index_limit: 1_000,
        row_cache_limit: 1_000,
        membership_limit: 1_000,
        ..Default::default()
    };

    let healthy = assess_pressure(&PressureInputs {
        disk: Some(DiskSpace {
            total_bytes: 1_000,
            available_bytes: 500,
        }),
        ..base.clone()
    });
    assert_eq!(healthy.level, PressureLevel::Normal);
    assert!(healthy.acquisition_error.is_none());

    let reclaimable = assess_pressure(&PressureInputs {
        disk: Some(DiskSpace {
            total_bytes: 1_000,
            available_bytes: 40,
        }),
        reclaimable_disk_bytes: 80,
        ..base.clone()
    });
    assert_eq!(reclaimable.level, PressureLevel::ReclaimCache);
    assert_eq!(reclaimable.reclaim, vec![CacheClass::DerivedIndex]);
    assert!(reclaimable.acquisition_error.is_none());
    assert!(
        reclaimable
            .explanation
            .contains("no captured record is evicted"),
        "{}",
        reclaimable.explanation
    );

    let squeezed = assess_pressure(&PressureInputs {
        disk: Some(DiskSpace {
            total_bytes: 1_000,
            available_bytes: 10,
        }),
        reclaimable_disk_bytes: 20,
        ..base.clone()
    });
    assert_eq!(squeezed.level, PressureLevel::Backpressure);
    assert!(squeezed.acquisition_error.is_none());

    let exhausted = assess_pressure(&PressureInputs {
        disk: Some(DiskSpace {
            total_bytes: 1_000,
            available_bytes: 1,
        }),
        reclaimable_disk_bytes: 0,
        ..base
    });
    assert_eq!(exhausted.level, PressureLevel::StopAcquisition);
    let error = exhausted
        .acquisition_error
        .clone()
        .expect("a visible acquisition error");
    assert!(error.contains("Acquisition is stopped"), "{error}");
    assert!(error.contains("are not captured"), "{error}");
    // Nothing durable may appear in a reclaim list at any level.
    for decision in [healthy, reclaimable, squeezed, exhausted] {
        assert!(
            decision
                .reclaim
                .iter()
                .all(|class| class.durability() == Durability::Disposable)
        );
    }
}

#[test]
fn durable_classes_never_report_reclaimable_space() {
    for class in [
        CacheClass::RawCapture,
        CacheClass::CommandEnrichment,
        CacheClass::InvestigationExport,
        CacheClass::Workspace,
    ] {
        let usage = CacheUsage::new(class, 4_096, None, 4_096);
        assert_eq!(usage.durability, Durability::Durable);
        assert_eq!(usage.reclaimable_bytes, 0, "{class:?}");
    }
    assert_eq!(
        CacheUsage::new(CacheClass::DerivedIndex, 4_096, Some(8_192), 1_024).reclaimable_bytes,
        1_024
    );
    // Command output is retained derived data, not a disposable cache.
    assert!(
        CacheClass::CommandEnrichment
            .note()
            .contains("not assumed reproducible")
    );
}

#[test]
fn a_missing_capture_root_reports_the_error_instead_of_an_empty_inventory() {
    let temp = root();
    let index = OwnershipIndex::build(
        &ScanRequest {
            root: temp.path().join("absent"),
            ..Default::default()
        },
        &AtomicBool::new(false),
    );
    assert!(index.captures.is_empty());
    assert_eq!(index.errors.len(), 1);
    assert!(
        index.errors[0].starts_with("capture root:"),
        "{:?}",
        index.errors
    );
}

#[test]
fn only_uuid_named_directories_are_treated_as_captures() {
    let temp = root();
    fs::create_dir_all(temp.path().join("not-a-capture")).unwrap();
    fs::write(temp.path().join("not-a-capture/data"), b"keep").unwrap();
    capture(temp.path(), "api", 2, 1_000);
    let index = index_of(temp.path());
    assert_eq!(index.captures.len(), 1);
    assert!(parse_source_id("not-a-capture").is_none());
    assert!(temp.path().join("not-a-capture/data").exists());
}
