//! Restarting a source the moment it reports a complete stop.
//!
//! `stop()` returning `complete` is the product's statement that the source is
//! finished and its journal is free; the application restarts sources on that
//! signal. Under load that statement is not always true: the journal lock is
//! still held and the restart fails with `Journal(AlreadyOpen)`. Two hundred
//! cycles reproduces it — around attempt 60 on a busy machine — where a handful
//! at rest never does.

use lvu_core::{Acquisition, SourceDefinition, SourceId};
use lvu_ingest::{RuntimeConfig, SourceManager};
use std::{collections::BTreeMap, fs, time::Duration};
use tempfile::TempDir;

fn definition(id: SourceId, path: &std::path::Path) -> SourceDefinition {
    SourceDefinition {
        schema_version: 1,
        id,
        name: "race".into(),
        acquisition: Acquisition::File {
            path: path.into(),
            follow: true,
        },
        identity_hints: BTreeMap::new(),
        retention: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "restart race, see TODO; run with --ignored or mise run test:restart-race"]
async fn stop_then_start_never_reports_the_journal_still_open() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("race.log");
    fs::write(&path, "one\ntwo\n").unwrap();
    let mut config = RuntimeConfig::default();
    config.acquisition.partial_flush_interval = Duration::from_secs(60);
    let manager = SourceManager::new(root.path().join("capture"), config).unwrap();
    let id = SourceId::new();
    let attempts: usize = std::env::var("LVU_RESTART_RACE_ATTEMPTS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(200);
    for attempt in 0..attempts {
        let handle = manager
            .start(definition(id, &path))
            .await
            .unwrap_or_else(|error| panic!("attempt {attempt}: start failed with {error:?}"));
        let report = handle.stop().await.unwrap();
        assert!(report.complete, "attempt {attempt}: stop was not complete");
    }
    manager.shutdown().await;
}
