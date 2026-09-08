//! Restarting a source the moment it reports a complete stop, while this
//! process is spawning subprocesses.
//!
//! `stop()` returning `complete` is the product's statement that the source is
//! finished and its journal is free; the application restarts sources on that
//! signal. It used not to be true under load, because the journal lock was an
//! `flock` and `fork` hands every child a duplicate of it — a child spawned by
//! a command source held the lock past the journal's own life, and the restart
//! was refused with `Journal(AlreadyOpen)`.
//!
//! The spawning threads are the test, not scenery: the same loop without them
//! passed 20 runs in 20 while this one failed 11 in 20 at load 23. Anything
//! that goes back to a descriptor-inherited lock fails here and nowhere else.

use lvu_core::{Acquisition, SourceDefinition, SourceId};
use lvu_ingest::{RuntimeConfig, SourceManager};
use std::{
    collections::BTreeMap,
    fs,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
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

/// Keeps a fork window open for as long as the guard lives.
struct Spawners {
    stop: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl Spawners {
    fn start(count: usize) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let threads = (0..count)
            .map(|_| {
                let stop = stop.clone();
                std::thread::spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        let _ = std::process::Command::new("/bin/true").status();
                    }
                })
            })
            .collect();
        Self { stop, threads }
    }
}

impl Drop for Spawners {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stop_then_start_never_reports_the_journal_still_open() {
    let _spawners = Spawners::start(4);
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
