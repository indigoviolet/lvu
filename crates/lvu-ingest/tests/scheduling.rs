//! Page reads interleave with capture without disturbing it.
//!
//! The writer thread owns the journal: it appends capture batches and serves
//! bounded page reads for live indexing and query workers. This exercises the
//! split schedule end to end by paging while a small-chunk file source is
//! still capturing, and every verdict is barrier-ordered rather than timed:
//! a progress watch proves which prefix had committed before a page was sent,
//! terminal states prove capture settled, and byte-exact replay proves nothing
//! was lost, duplicated or reordered. Fairness itself — pages overtaking
//! queued batches, and capture progressing under sustained pages — is proved
//! by the prestaged channel-ordering unit tests in `writer.rs`, where the
//! whole input is fixed before the writer starts and no assertion can depend
//! on scheduling speed at all.

use lvu_core::{Acquisition, SourceDefinition, SourceId};
use lvu_ingest::{RuntimeConfig, RuntimeState, SourceManager};
use std::{collections::BTreeMap, fs, time::Duration};

fn definition(id: SourceId, path: &std::path::Path) -> SourceDefinition {
    SourceDefinition {
        schema_version: 1,
        id,
        name: "scheduling fixture".into(),
        acquisition: Acquisition::File {
            path: path.into(),
            follow: false,
        },
        identity_hints: BTreeMap::new(),
        retention: None,
    }
}

async fn wait_for_stopped(handle: &lvu_ingest::SourceHandle) {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if handle.progress().state.is_terminal() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("capture settled without hanging");
}

async fn replay(handle: &lvu_ingest::SourceHandle) -> Vec<u8> {
    let mut offset = 0;
    let mut replayed = Vec::new();
    loop {
        let page = handle
            .read_page(offset, 4096, 4 * 1024 * 1024)
            .await
            .expect("replay page");
        for record in &page.records {
            replayed.extend_from_slice(&record.bytes);
            replayed.extend_from_slice(&record.delimiter);
        }
        if page.end_of_journal {
            return replayed;
        }
        offset = page.next_offset;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_pages_read_committed_prefix_and_capture_completes_whole() {
    let root = tempfile::tempdir().expect("temporary root");
    let input = root.path().join("input.log");
    // Small reads turn a short file into dozens of capture events, so pages
    // sent mid-capture meet a busy writer however the threads interleave.
    let mut fixture = String::new();
    for index in 0..200 {
        fixture.push_str(&format!("line-{index:04}\n"));
    }
    fs::write(&input, fixture.as_bytes()).expect("fixture file");

    let mut config = RuntimeConfig::default();
    config.acquisition.read_chunk_bytes = 64;
    let capture_root = root.path().join("capture");
    let source_id = SourceId::new();
    let manager = SourceManager::new(&capture_root, config).expect("manager");
    let handle = manager
        .start(definition(source_id, &input))
        .await
        .expect("start");

    // Barrier, not timing: the first five records are committed before any
    // page below is sent, so every page necessarily contains them as its
    // prefix however the writer interleaves the remaining flood.
    let mut progress = handle.subscribe();
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if progress.borrow_and_update().records >= 5 {
                return;
            }
            progress.changed().await.expect("progress channel");
        }
    })
    .await
    .expect("committed prefix without hanging");

    for _ in 0..5 {
        let page = tokio::time::timeout(Duration::from_secs(60), handle.read_page(0, 5, 4096))
            .await
            .expect("page resolved without hanging")
            .expect("page read");
        assert_eq!(page.records.len(), 5);
        assert_eq!(page.records[0].bytes.as_slice(), b"line-0000".as_slice());
        assert_eq!(page.records[4].bytes.as_slice(), b"line-0004".as_slice());
    }

    // The flood still completes whole: ordering, bytes and durability hold.
    wait_for_stopped(&handle).await;
    assert_eq!(handle.progress().state, RuntimeState::Stopped);
    assert_eq!(replay(&handle).await, fixture.as_bytes());
    assert_eq!(
        handle.progress().synced_records,
        handle.progress().records,
        "every captured record must be durable at clean stop"
    );
    let settled_records = handle.progress().records;
    drop(handle);
    drop(manager);

    // Reopening resumes from the durable cursor: nothing re-read, nothing
    // lost, so the cursor survived the interleaved paging untouched.
    let manager = SourceManager::new(&capture_root, RuntimeConfig::default()).unwrap();
    let reopened = manager.start(definition(source_id, &input)).await.unwrap();
    wait_for_stopped(&reopened).await;
    assert_eq!(reopened.progress().state, RuntimeState::Stopped);
    assert_eq!(reopened.progress().records, settled_records);
    assert_eq!(replay(&reopened).await, fixture.as_bytes());
}
