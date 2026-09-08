use fs2::FileExt;
use lvu::{
    Action, App, RowProvider, ViewportRequest,
    app::{SourceItem, ViewItem},
};
use lvu_core::{
    Acquisition, CommandDefinition, CommandProgram, RestartPolicy, SourceDefinition, SourceId,
};
use lvu_ingest::{RuntimeConfig, RuntimeState, SourceHandle, SourceManager};
use lvu_live::{AdapterError, IndexState, LiveConfig, LiveRowProvider};
use std::{collections::BTreeMap, fs, io::Write, time::Duration};
use tempfile::TempDir;

fn runtime_config() -> RuntimeConfig {
    let mut config = RuntimeConfig::default();
    config.acquisition.channel_capacity = 8;
    config.acquisition.read_chunk_bytes = 64;
    config.acquisition.maximum_record_bytes = 64;
    config.acquisition.partial_flush_interval = Duration::from_millis(20);
    config.writer_queue_capacity = 8;
    config.batch_records = 8;
    config.max_page_records = 32;
    config.max_page_bytes = 4096;
    config.graceful_stop_deadline = Duration::from_secs(2);
    config
}

fn live_config(root: &TempDir) -> LiveConfig {
    let mut config = LiveConfig::new(root.path().join("derived"));
    config.index_page_records = 16;
    config.index_page_bytes = 2048;
    config.maximum_request_rows = 16;
    config.request_queue_capacity = 4;
    config.update_queue_capacity = 16;
    config.cache_rows = 8;
    config.cache_bytes = 2048;
    config
}

fn file_source(id: SourceId, path: &std::path::Path, follow: bool) -> SourceDefinition {
    SourceDefinition {
        schema_version: 1,
        id,
        name: "file fixture".into(),
        acquisition: Acquisition::File {
            path: path.to_owned(),
            follow,
        },
        identity_hints: BTreeMap::new(),
        retention: None,
    }
}

fn command_source(id: SourceId, script: &str) -> SourceDefinition {
    SourceDefinition {
        schema_version: 1,
        id,
        name: "command fixture".into(),
        acquisition: Acquisition::Command {
            command: CommandDefinition {
                program: CommandProgram::Exec {
                    executable: "sh".into(),
                    args: vec!["-c".into(), script.into()],
                },
                cwd: None,
                environment: BTreeMap::new(),
                restart: RestartPolicy::Never,
            },
        },
        identity_hints: BTreeMap::new(),
        retention: None,
    }
}

async fn wait_runtime(handle: &SourceHandle, predicate: impl Fn(RuntimeState, u64) -> bool) {
    let mut progress = handle.subscribe();
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            let current = progress.borrow().clone();
            if predicate(current.state, current.records) {
                break;
            }
            progress.changed().await.expect("runtime progress closed");
        }
    })
    .await
    .expect("runtime wait timed out");
}

async fn wait_index(provider: &LiveRowProvider, source: SourceId, count: u64) {
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            provider.drain_ready_updates(64);
            if provider
                .source_status(source)
                .is_some_and(|status| status.indexed_records >= count)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("index wait timed out");
}

async fn wait_page(
    provider: &LiveRowProvider,
    view: &str,
    start: usize,
    len: usize,
) -> Vec<lvu::DisplayRow> {
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            provider.drain_ready_updates(64);
            let page = provider.page(view, ViewportRequest { start, len });
            if page.rows.len() == len.min(page.total.saturating_sub(start)) && !page.rows.is_empty()
            {
                break page.rows;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("page wait timed out")
}

#[tokio::test]
async fn live_file_arrival_is_nonblocking_and_selection_survives_tail_growth() {
    let root = TempDir::new().unwrap();
    let input = root.path().join("live.log");
    fs::write(&input, b"first\ninitial-second\n").unwrap();
    let id = SourceId::new();
    let capture_root = root.path().join("capture");
    let manager = SourceManager::new(&capture_root, runtime_config()).unwrap();
    let handle = manager.start(file_source(id, &input, true)).await.unwrap();
    let provider = LiveRowProvider::new(live_config(&root)).unwrap();
    provider.register_source(handle.clone()).unwrap();
    provider.register_raw_view("raw", vec![id]).unwrap();
    let initial = provider.page("raw", ViewportRequest { start: 0, len: 5 });
    assert!(
        initial.rows.is_empty(),
        "UI call must return loading rather than await disk"
    );
    wait_index(&provider, id, 2).await;
    let rows = wait_page(&provider, "raw", 0, 2).await;
    assert_eq!(rows[0].text, "first");
    let selected = rows[0].id.clone();

    let mut app = App::new(
        vec![SourceItem {
            id: id.0.to_string(),
            name: "live".into(),
            health: "running".into(),
        }],
        vec![ViewItem {
            id: "raw".into(),
            source_id: id.0.to_string(),
            name: "raw".into(),
        }],
        false,
    );
    app.sync_provider(&provider, 5);
    app.handle(Action::ToggleFollow, &provider);
    app.handle(Action::Top, &provider);
    fs::OpenOptions::new()
        .append(true)
        .open(&input)
        .unwrap()
        .write_all(b"second\n")
        .unwrap();
    wait_index(&provider, id, 3).await;
    provider.drain_ready_updates(64);
    app.sync_provider(&provider, 5);
    assert_eq!(app.view_state().unwrap().selected.as_ref(), Some(&selected));
    assert_eq!(provider.index_of_id("raw", &selected), Some(0));
    assert_eq!(wait_page(&provider, "raw", 2, 1).await[0].text, "second");
    handle.stop().await.unwrap();
    provider.shutdown().await;
}

#[tokio::test]
async fn command_invalid_utf8_partial_records_and_empty_indexing_are_explicit() {
    let root = TempDir::new().unwrap();
    let id = SourceId::new();
    let capture_root = root.path().join("capture");
    let manager = SourceManager::new(&capture_root, runtime_config()).unwrap();
    let handle = manager
        .start(command_source(
            id,
            "printf '\\377bad-more'; sleep .08; printf '\\n'; sleep .2",
        ))
        .await
        .unwrap();
    let mut config = live_config(&root);
    config.maximum_display_bytes = 4;
    let provider = LiveRowProvider::new(config).unwrap();
    provider.register_source(handle.clone()).unwrap();
    provider.register_raw_view("raw", vec![id]).unwrap();
    let status = provider.view_status("raw").unwrap();
    assert_eq!(status.indexed_physical_records, 0);
    assert!(status.indexing);
    wait_index(&provider, id, 1).await;
    let row = wait_page(&provider, "raw", 0, 1).await.remove(0);
    assert!(row.text.contains('\u{fffd}'));
    assert_eq!(row.level, "fragment");
    assert!(row.details.iter().any(|(key, _)| key == "fragment"));
    assert!(
        row.details
            .iter()
            .any(|(key, _)| key == "display_truncated_bytes")
    );
    wait_runtime(&handle, |state, _| state == RuntimeState::Stopped).await;
    provider.shutdown().await;
}

#[tokio::test]
async fn large_history_pages_backwards_with_cache_independent_of_total() {
    let root = TempDir::new().unwrap();
    let input = root.path().join("large.log");
    let fixture = (0..600)
        .map(|index| format!("row-{index:04}\n"))
        .collect::<String>();
    fs::write(&input, fixture).unwrap();
    let id = SourceId::new();
    let mut runtime = runtime_config();
    runtime.acquisition.partial_flush_interval = Duration::from_secs(10);
    let manager = SourceManager::new(root.path().join("capture"), runtime).unwrap();
    let handle = manager.start(file_source(id, &input, false)).await.unwrap();
    wait_runtime(&handle, |state, _| state == RuntimeState::Stopped).await;
    let mut config = live_config(&root);
    config.cache_rows = 6;
    config.cache_bytes = 1024;
    let provider = LiveRowProvider::new(config).unwrap();
    provider.register_source(handle.clone()).unwrap();
    provider.register_raw_view("raw", vec![id]).unwrap();
    wait_index(&provider, id, 600).await;
    let tail = wait_page(&provider, "raw", 594, 6).await;
    assert_eq!(tail[5].text, "row-0599");
    let tail_id = tail[5].id.clone();
    assert_eq!(wait_page(&provider, "raw", 0, 6).await[0].text, "row-0000");
    assert!(provider.row_by_id("raw", &tail_id).is_none());
    let loaded_by_id = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            provider.drain_ready_updates(64);
            if let Some(row) = provider.row_by_id("raw", &tail_id) {
                break row;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("row identity lookup timed out");
    assert_eq!(loaded_by_id.text, "row-0599");
    assert_eq!(provider.index_of_id("raw", &tail_id), Some(599));
    for start in 100..150 {
        let _ = provider.page("raw", ViewportRequest { start, len: 1 });
    }
    let stats = provider.stats();
    assert!(stats.cached_rows <= 6);
    assert!(stats.cached_bytes <= 1024);
    assert!(stats.pending_requests <= 21);
    assert_eq!(
        provider
            .page("raw", ViewportRequest { start: 0, len: 0 })
            .total,
        600
    );
    provider.shutdown().await;
}

#[tokio::test]
async fn derived_index_disk_limit_is_explicit_and_does_not_change_journal() {
    let root = TempDir::new().unwrap();
    let input = root.path().join("limited.log");
    fs::write(&input, b"0\n1\n2\n3\n").unwrap();
    let id = SourceId::new();
    let capture_root = root.path().join("capture");
    let manager = SourceManager::new(&capture_root, runtime_config()).unwrap();
    let handle = manager.start(file_source(id, &input, false)).await.unwrap();
    wait_runtime(&handle, |state, _| state == RuntimeState::Stopped).await;
    let journal = capture_root.join(id.0.to_string()).join("capture.journal");
    let original = fs::read(&journal).unwrap();
    let mut config = live_config(&root);
    config.index_page_records = 1;
    config.maximum_index_bytes_per_source = 140;
    config.cache_rows = 1;
    let provider = LiveRowProvider::new(config).unwrap();
    provider.register_source(handle.clone()).unwrap();
    provider.register_raw_view("raw", vec![id]).unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            provider.drain_ready_updates(64);
            if provider.source_status(id).unwrap().index == IndexState::Limited {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("index limit status timed out");
    let status = provider.source_status(id).unwrap();
    assert_eq!(status.indexed_records, 2);
    assert!(status.last_error.unwrap().contains("byte limit"));
    assert!(fs::metadata(provider.index_path(id)).unwrap().len() <= 140);
    assert_eq!(fs::read(journal).unwrap(), original);
    assert_eq!(wait_page(&provider, "raw", 0, 1).await[0].text, "0");
    assert_eq!(wait_page(&provider, "raw", 1, 1).await[0].text, "1");
    assert_eq!(wait_page(&provider, "raw", 0, 1).await[0].text, "0");
    assert_eq!(
        provider.source_status(id).unwrap().index,
        IndexState::Limited
    );
    provider.shutdown().await;
}

#[tokio::test]
async fn two_views_share_one_source_worker_and_cache() {
    let root = TempDir::new().unwrap();
    let input = root.path().join("shared.log");
    fs::write(&input, b"one\ntwo\nthree\n").unwrap();
    let id = SourceId::new();
    let manager = SourceManager::new(root.path().join("capture"), runtime_config()).unwrap();
    let handle = manager.start(file_source(id, &input, false)).await.unwrap();
    wait_runtime(&handle, |state, _| state == RuntimeState::Stopped).await;
    let provider = LiveRowProvider::new(live_config(&root)).unwrap();
    provider.register_source(handle.clone()).unwrap();
    provider.register_raw_view("a", vec![id]).unwrap();
    provider.register_raw_view("b", vec![id]).unwrap();
    wait_index(&provider, id, 3).await;
    let a = wait_page(&provider, "a", 0, 3).await;
    let completed = provider.stats().completed_requests;
    let b = provider.page("b", ViewportRequest { start: 0, len: 3 });
    assert_eq!(a, b.rows);
    assert_eq!(provider.stats().completed_requests, completed);
    provider.shutdown().await;
}

#[tokio::test]
async fn one_raw_view_pages_multiple_source_journals_with_stable_ids() {
    let root = TempDir::new().unwrap();
    let first_path = root.path().join("first.log");
    let second_path = root.path().join("second.log");
    fs::write(&first_path, b"a1\na2\n").unwrap();
    fs::write(&second_path, b"b1\n").unwrap();
    let first_id = SourceId::new();
    let second_id = SourceId::new();
    let manager = SourceManager::new(root.path().join("capture"), runtime_config()).unwrap();
    let first = manager
        .start(file_source(first_id, &first_path, false))
        .await
        .unwrap();
    let second = manager
        .start(file_source(second_id, &second_path, false))
        .await
        .unwrap();
    wait_runtime(&first, |state, _| state == RuntimeState::Stopped).await;
    wait_runtime(&second, |state, _| state == RuntimeState::Stopped).await;
    let provider = LiveRowProvider::new(live_config(&root)).unwrap();
    provider.register_source(first).unwrap();
    provider.register_source(second).unwrap();
    provider
        .register_raw_view("both", vec![first_id, second_id])
        .unwrap();
    wait_index(&provider, first_id, 2).await;
    wait_index(&provider, second_id, 1).await;
    let rows = wait_page(&provider, "both", 0, 3).await;
    assert_eq!(
        rows.iter().map(|row| row.text.as_str()).collect::<Vec<_>>(),
        ["a1", "a2", "b1"]
    );
    assert_eq!(rows[0].id.source_id, first_id.0.to_string());
    assert_eq!(rows[2].id.source_id, second_id.0.to_string());
    assert_eq!(provider.index_of_id("both", &rows[2].id), Some(2));
    provider.shutdown().await;
}

#[tokio::test]
async fn source_generation_replacement_fences_stale_cache_and_rebuilds() {
    let root = TempDir::new().unwrap();
    let input = root.path().join("restart.log");
    fs::write(&input, b"old\n").unwrap();
    let id = SourceId::new();
    let manager = SourceManager::new(root.path().join("capture"), runtime_config()).unwrap();
    let first = manager.start(file_source(id, &input, false)).await.unwrap();
    wait_runtime(&first, |state, _| state == RuntimeState::Stopped).await;
    let provider = LiveRowProvider::new(live_config(&root)).unwrap();
    provider.register_source(first.clone()).unwrap();
    provider.register_raw_view("raw", vec![id]).unwrap();
    wait_index(&provider, id, 1).await;
    assert_eq!(wait_page(&provider, "raw", 0, 1).await[0].text, "old");
    let first_generation = provider.source_status(id).unwrap().generation;

    fs::write(&input, b"new\n").unwrap();
    let second = manager.start(file_source(id, &input, false)).await.unwrap();
    wait_runtime(&second, |state, _| state == RuntimeState::Stopped).await;
    provider.register_source(second).unwrap();
    assert!(
        provider
            .page("raw", ViewportRequest { start: 0, len: 1 })
            .rows
            .is_empty()
    );
    wait_index(&provider, id, 2).await;
    let status = provider.source_status(id).unwrap();
    assert!(status.generation > first_generation);
    let rows = wait_page(&provider, "raw", 0, 2).await;
    assert_eq!(
        rows.iter().map(|row| row.text.as_str()).collect::<Vec<_>>(),
        ["old", "new"]
    );
    assert_eq!(rows[0].id.sequence, 0);
    provider.shutdown().await;
}

#[tokio::test]
async fn corrupt_index_rebuilds_without_changing_journal() {
    let root = TempDir::new().unwrap();
    let input = root.path().join("rebuild.log");
    fs::write(&input, b"alpha\nbeta\n").unwrap();
    let id = SourceId::new();
    let manager = SourceManager::new(root.path().join("capture"), runtime_config()).unwrap();
    let handle = manager.start(file_source(id, &input, false)).await.unwrap();
    wait_runtime(&handle, |state, _| state == RuntimeState::Stopped).await;
    let provider = LiveRowProvider::new(live_config(&root)).unwrap();
    provider.register_source(handle.clone()).unwrap();
    provider.register_raw_view("raw", vec![id]).unwrap();
    wait_index(&provider, id, 2).await;
    provider.shutdown().await;
    let journal = root
        .path()
        .join("capture")
        .join(id.0.to_string())
        .join("capture.journal");
    let before = fs::read(&journal).unwrap();
    fs::write(provider.index_path(id), b"corrupt derived index").unwrap();

    let provider = LiveRowProvider::new(live_config(&root)).unwrap();
    provider.register_source(handle).unwrap();
    provider.register_raw_view("raw", vec![id]).unwrap();
    wait_index(&provider, id, 2).await;
    assert_eq!(
        wait_page(&provider, "raw", 0, 2)
            .await
            .iter()
            .map(|row| row.text.as_str())
            .collect::<Vec<_>>(),
        ["alpha", "beta"]
    );
    assert_eq!(fs::read(journal).unwrap(), before);
    assert_ne!(provider.source_status(id).unwrap().index, IndexState::Error);
    provider.shutdown().await;
}

#[tokio::test]
async fn shutdown_cancels_workers_with_full_bounded_queues() {
    let root = TempDir::new().unwrap();
    let input = root.path().join("shutdown.log");
    fs::write(
        &input,
        (0..100).map(|i| format!("{i}\n")).collect::<String>(),
    )
    .unwrap();
    let id = SourceId::new();
    let manager = SourceManager::new(root.path().join("capture"), runtime_config()).unwrap();
    let handle = manager.start(file_source(id, &input, false)).await.unwrap();
    wait_runtime(&handle, |state, _| state == RuntimeState::Stopped).await;
    let mut config = live_config(&root);
    config.update_queue_capacity = 1;
    config.request_queue_capacity = 1;
    let provider = LiveRowProvider::new(config).unwrap();
    provider.register_source(handle.clone()).unwrap();
    provider.register_raw_view("raw", vec![id]).unwrap();
    for start in 0..20 {
        let _ = provider.page("raw", ViewportRequest { start, len: 1 });
    }
    tokio::time::timeout(Duration::from_secs(1), provider.shutdown())
        .await
        .expect("adapter shutdown blocked on full queues");
    assert!(matches!(
        provider.register_source(handle),
        Err(AdapterError::Closed)
    ));
    assert!(matches!(
        provider.register_raw_view("closed", vec![id]),
        Err(AdapterError::Closed)
    ));
    assert_eq!(provider.stats().pending_requests, 0);
}

#[tokio::test]
async fn viewport_request_is_served_before_large_history_finishes_indexing() {
    let root = TempDir::new().unwrap();
    let input = root.path().join("fair.log");
    fs::write(
        &input,
        (0..2_000)
            .map(|index| format!("fair-{index}\n"))
            .collect::<String>(),
    )
    .unwrap();
    let id = SourceId::new();
    let manager = SourceManager::new(root.path().join("capture"), runtime_config()).unwrap();
    let handle = manager.start(file_source(id, &input, false)).await.unwrap();
    wait_runtime(&handle, |state, _| state == RuntimeState::Stopped).await;
    let mut config = live_config(&root);
    config.index_page_records = 1;
    config.update_queue_capacity = 2;
    let provider = LiveRowProvider::new(config).unwrap();
    provider.register_source(handle).unwrap();
    provider.register_raw_view("raw", vec![id]).unwrap();

    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            provider.drain_ready_updates(1);
            let status = provider.source_status(id).unwrap();
            if status.indexed_records > 0 {
                let page = provider.page("raw", ViewportRequest { start: 0, len: 1 });
                if page.rows.first().is_some_and(|row| row.text == "fair-0") {
                    assert!(status.indexed_records < status.reported_records);
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("viewport request starved behind indexing");
    provider.shutdown().await;
}

#[tokio::test]
async fn valid_partial_page_suffix_is_rolled_back_and_reindexed() {
    let root = TempDir::new().unwrap();
    let input = root.path().join("partial-index.log");
    fs::write(&input, b"zero\none\ntwo\nthree\n").unwrap();
    let id = SourceId::new();
    let manager = SourceManager::new(root.path().join("capture"), runtime_config()).unwrap();
    let handle = manager.start(file_source(id, &input, false)).await.unwrap();
    wait_runtime(&handle, |state, _| state == RuntimeState::Stopped).await;
    let mut config = live_config(&root);
    config.index_page_records = 4;
    let first = LiveRowProvider::new(config.clone()).unwrap();
    first.register_source(handle.clone()).unwrap();
    wait_index(&first, id, 4).await;
    let artifact = first.index_path(id);
    first.shutdown().await;

    let complete_length = fs::metadata(&artifact).unwrap().len();
    assert_eq!(complete_length, 60 + 4 * 40);
    fs::OpenOptions::new()
        .write(true)
        .open(&artifact)
        .unwrap()
        .set_len(60 + 40)
        .unwrap();

    let second = LiveRowProvider::new(config).unwrap();
    second.register_source(handle).unwrap();
    second.register_raw_view("raw", vec![id]).unwrap();
    wait_index(&second, id, 4).await;
    assert_eq!(
        wait_page(&second, "raw", 0, 4)
            .await
            .iter()
            .map(|row| row.text.as_str())
            .collect::<Vec<_>>(),
        ["zero", "one", "two", "three"]
    );
    second.shutdown().await;
}

#[tokio::test]
async fn changed_page_budget_rebuilds_index_before_serving_history() {
    let root = TempDir::new().unwrap();
    let input = root.path().join("page-budget.log");
    fs::write(&input, b"one\ntwo\nthree\nfour\n").unwrap();
    let id = SourceId::new();
    let manager = SourceManager::new(root.path().join("capture"), runtime_config()).unwrap();
    let handle = manager.start(file_source(id, &input, false)).await.unwrap();
    wait_runtime(&handle, |state, _| state == RuntimeState::Stopped).await;
    let mut config = live_config(&root);
    config.index_page_records = 4;
    let first = LiveRowProvider::new(config.clone()).unwrap();
    first.register_source(handle.clone()).unwrap();
    wait_index(&first, id, 4).await;
    first.shutdown().await;

    config.index_page_bytes = 1;
    let second = LiveRowProvider::new(config).unwrap();
    second.register_source(handle).unwrap();
    second.register_raw_view("raw", vec![id]).unwrap();
    wait_index(&second, id, 4).await;
    assert_eq!(wait_page(&second, "raw", 3, 1).await[0].text, "four");
    second.shutdown().await;
}

#[tokio::test]
async fn registration_is_idempotent_and_index_has_single_provider_owner() {
    let root = TempDir::new().unwrap();
    let input = root.path().join("owned.log");
    fs::write(&input, b"owned\n").unwrap();
    let id = SourceId::new();
    let manager = SourceManager::new(root.path().join("capture"), runtime_config()).unwrap();
    let handle = manager.start(file_source(id, &input, false)).await.unwrap();
    wait_runtime(&handle, |state, _| state == RuntimeState::Stopped).await;
    let config = live_config(&root);
    let owner = LiveRowProvider::new(config.clone()).unwrap();
    owner.register_source(handle.clone()).unwrap();
    owner.register_source(handle.clone()).unwrap();
    owner.register_raw_view("raw", vec![id]).unwrap();
    wait_index(&owner, id, 1).await;

    // A second provider must never write the same index concurrently. It waits
    // for the owner instead of taking it, and instead of giving up: the wait is
    // what makes an ordinary shutdown race survivable.
    let contender = LiveRowProvider::new(config).unwrap();
    contender.register_source(handle).unwrap();
    contender.register_raw_view("contender", vec![id]).unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            contender.drain_ready_updates(8);
            if contender.source_status(id).unwrap().index == IndexState::Contended {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("overlapping index owner was not made to wait");
    assert!(
        contender
            .source_status(id)
            .unwrap()
            .last_error
            .unwrap()
            .contains("already owned")
    );
    assert_eq!(wait_page(&owner, "raw", 0, 1).await[0].text, "owned");

    owner.shutdown().await;
    assert_eq!(
        wait_page(&contender, "contender", 0, 1).await[0].text,
        "owned",
        "the waiting provider must take over once the owner releases the index"
    );
    contender.shutdown().await;
}

#[tokio::test]
async fn invalid_utf8_projection_fits_tiny_cache_and_remains_displayable() {
    let root = TempDir::new().unwrap();
    let input = root.path().join("invalid.log");
    let mut bytes = vec![0xff; 64 * 1024];
    bytes.push(b'\n');
    fs::write(&input, bytes).unwrap();
    let id = SourceId::new();
    let mut runtime = runtime_config();
    runtime.acquisition.read_chunk_bytes = 128 * 1024;
    runtime.acquisition.maximum_record_bytes = 128 * 1024;
    runtime.max_page_bytes = 256 * 1024;
    let manager = SourceManager::new(root.path().join("capture"), runtime).unwrap();
    let handle = manager.start(file_source(id, &input, false)).await.unwrap();
    wait_runtime(&handle, |state, _| state == RuntimeState::Stopped).await;
    let mut config = live_config(&root);
    config.cache_bytes = 256;
    config.maximum_display_bytes = 1024;
    let provider = LiveRowProvider::new(config).unwrap();
    provider.register_source(handle).unwrap();
    provider.register_raw_view("raw", vec![id]).unwrap();
    wait_index(&provider, id, 1).await;
    let row = wait_page(&provider, "raw", 0, 1).await.remove(0);
    assert!(row.text.contains('\u{fffd}'));
    assert!(
        row.details
            .iter()
            .any(|(key, _)| key == "display_truncated_decoded_bytes")
    );
    assert!(provider.stats().cached_bytes <= 256);
    assert_eq!(provider.stats().cached_rows, 1);
    provider.shutdown().await;
}

fn derived_index_bytes(directory: &std::path::Path) -> u64 {
    fs::read_dir(directory)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".rows.idx"))
        .map(|entry| entry.metadata().unwrap().len())
        .sum()
}

#[tokio::test]
async fn bounded_unverified_reconciliation_is_reported_without_deleting_unknown_files() {
    let root = TempDir::new().unwrap();
    let derived = root.path().join("derived");
    fs::create_dir_all(&derived).unwrap();
    for index in 0..65 {
        fs::write(
            derived.join(format!("unknown-{index}.rows.idx")),
            [index as u8],
        )
        .unwrap();
    }
    let input = root.path().join("unverified.log");
    fs::write(&input, b"captured\n").unwrap();
    let id = SourceId::new();
    let manager = SourceManager::new(root.path().join("capture"), runtime_config()).unwrap();
    let handle = manager.start(file_source(id, &input, false)).await.unwrap();
    wait_runtime(&handle, |state, _| state == RuntimeState::Stopped).await;
    let mut config = live_config(&root);
    config.maximum_sources = 1;
    assert_eq!(
        LiveConfig::new(&derived).maximum_total_index_bytes,
        5 * 1024 * 1024 * 1024
    );
    let provider = LiveRowProvider::new(config).unwrap();
    provider.register_source(handle).unwrap();
    provider.register_raw_view("raw", vec![id]).unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            provider.drain_ready_updates(16);
            if provider.source_status(id).unwrap().index == IndexState::BudgetUnverified {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("unverified bounded reconciliation was not reported");
    assert!(
        provider
            .source_status(id)
            .unwrap()
            .last_error
            .unwrap()
            .contains("unverified")
    );
    // The unaccountable total suspends the aggregate guarantee. It must not
    // suspend the product: this source's own rows are still served under its
    // own exact cap.
    assert_eq!(wait_page(&provider, "raw", 0, 1).await[0].text, "captured");
    assert_eq!(
        fs::read_dir(&derived)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("unknown-"))
            .count(),
        65
    );
    provider.shutdown().await;
}

#[tokio::test]
async fn global_index_budget_bounds_concurrent_source_growth_and_preserves_capture() {
    let root = TempDir::new().unwrap();
    let capture = root.path().join("capture");
    let first_file = root.path().join("first-budget.log");
    let second_file = root.path().join("second-budget.log");
    fs::write(&first_file, b"a0\na1\na2\na3\n").unwrap();
    fs::write(&second_file, b"b0\nb1\nb2\nb3\n").unwrap();
    let first_id = SourceId::new();
    let second_id = SourceId::new();
    let manager = SourceManager::new(&capture, runtime_config()).unwrap();
    let first = manager
        .start(file_source(first_id, &first_file, false))
        .await
        .unwrap();
    let second = manager
        .start(file_source(second_id, &second_file, false))
        .await
        .unwrap();
    wait_runtime(&first, |state, _| state == RuntimeState::Stopped).await;
    wait_runtime(&second, |state, _| state == RuntimeState::Stopped).await;
    let first_journal = capture.join(first_id.0.to_string()).join("capture.journal");
    let second_journal = capture
        .join(second_id.0.to_string())
        .join("capture.journal");
    let first_raw = fs::read(&first_journal).unwrap();
    let second_raw = fs::read(&second_journal).unwrap();

    let mut config = live_config(&root);
    config.index_page_records = 1;
    config.maximum_index_bytes_per_source = 1024;
    config.maximum_total_index_bytes = 240;
    let provider = LiveRowProvider::new(config).unwrap();
    provider.register_source(first).unwrap();
    provider.register_source(second).unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            provider.drain_ready_updates(64);
            if [first_id, second_id].iter().any(|id| {
                provider
                    .source_status(*id)
                    .is_some_and(|status| status.index == IndexState::Limited)
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("global index limit was not reported");
    let statuses = [
        provider.source_status(first_id).unwrap(),
        provider.source_status(second_id).unwrap(),
    ];
    assert!(statuses.iter().all(|status| status.reported_records == 4));
    assert!(statuses.iter().any(|status| {
        status.index == IndexState::Limited
            && status
                .last_error
                .as_deref()
                .is_some_and(|message| message.contains("global derived-index budget"))
    }));
    assert!(derived_index_bytes(&root.path().join("derived")) <= 240);
    assert_eq!(fs::read(first_journal).unwrap(), first_raw);
    assert_eq!(fs::read(second_journal).unwrap(), second_raw);
    provider.shutdown().await;
}

#[tokio::test]
async fn unused_cleanup_releases_global_budget_for_a_later_index() {
    let root = TempDir::new().unwrap();
    let capture = root.path().join("capture");
    let first_file = root.path().join("cleanup-first.log");
    let second_file = root.path().join("cleanup-second.log");
    fs::write(&first_file, b"first\n").unwrap();
    fs::write(&second_file, b"second\n").unwrap();
    let first_id = SourceId::new();
    let second_id = SourceId::new();
    let manager = SourceManager::new(&capture, runtime_config()).unwrap();
    let first = manager
        .start(file_source(first_id, &first_file, false))
        .await
        .unwrap();
    let second = manager
        .start(file_source(second_id, &second_file, false))
        .await
        .unwrap();
    wait_runtime(&first, |state, _| state == RuntimeState::Stopped).await;
    wait_runtime(&second, |state, _| state == RuntimeState::Stopped).await;
    let mut config = live_config(&root);
    config.index_page_records = 1;
    config.maximum_total_index_bytes = 100;

    let original = LiveRowProvider::new(config.clone()).unwrap();
    original.register_source(first).unwrap();
    wait_index(&original, first_id, 1).await;
    let old_path = original.index_path(first_id);
    original.shutdown().await;

    let cleanup = LiveRowProvider::new(config.clone()).unwrap();
    let identity = match cleanup.inspect_derived_artifact(&old_path).unwrap() {
        lvu_live::DerivedArtifactStatus::Unused { identity, .. } => identity,
        status => panic!("expected unused owned index, got {status:?}"),
    };
    assert_eq!(
        cleanup.remove_unused_derived_artifact(&identity).unwrap(),
        100
    );

    let replacement = LiveRowProvider::new(config).unwrap();
    replacement.register_source(second).unwrap();
    wait_index(&replacement, second_id, 1).await;
    assert_eq!(derived_index_bytes(&root.path().join("derived")), 100);
    replacement.shutdown().await;
    cleanup.shutdown().await;
}

#[tokio::test]
async fn capture_continues_after_global_index_limit_and_indexed_history_remains_readable() {
    let root = TempDir::new().unwrap();
    let input = root.path().join("global-live.log");
    fs::write(&input, b"kept\n").unwrap();
    let id = SourceId::new();
    let capture = root.path().join("capture");
    let manager = SourceManager::new(&capture, runtime_config()).unwrap();
    let handle = manager.start(file_source(id, &input, true)).await.unwrap();
    wait_runtime(&handle, |_, records| records >= 1).await;
    let mut config = live_config(&root);
    config.index_page_records = 1;
    config.maximum_total_index_bytes = 100;
    let provider = LiveRowProvider::new(config).unwrap();
    provider.register_source(handle.clone()).unwrap();
    provider.register_raw_view("raw", vec![id]).unwrap();
    wait_index(&provider, id, 1).await;

    let mut writer = fs::OpenOptions::new().append(true).open(&input).unwrap();
    writer.write_all(b"not-indexed\nstill-captured\n").unwrap();
    wait_runtime(&handle, |_, records| records >= 3).await;
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            provider.drain_ready_updates(32);
            if provider.source_status(id).unwrap().index == IndexState::Limited {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("global limit was not reported after continued capture");
    assert_eq!(wait_page(&provider, "raw", 0, 1).await[0].text, "kept");
    let status = provider.source_status(id).unwrap();
    assert_eq!(status.indexed_records, 1);
    assert!(status.reported_records >= 3);
    let journal = capture.join(id.0.to_string()).join("capture.journal");
    let captured = fs::read(&journal).unwrap();
    handle.stop().await.unwrap();
    provider.shutdown().await;
    assert_eq!(fs::read(journal).unwrap(), captured);
}

#[tokio::test]
async fn restart_counts_existing_indexes_and_cross_provider_reservations_do_not_oversubscribe() {
    let root = TempDir::new().unwrap();
    let capture = root.path().join("capture");
    let files = [
        root.path().join("race-a.log"),
        root.path().join("race-b.log"),
    ];
    fs::write(&files[0], b"a\n").unwrap();
    fs::write(&files[1], b"b\n").unwrap();
    let ids = [SourceId::new(), SourceId::new()];
    let manager = SourceManager::new(&capture, runtime_config()).unwrap();
    let a = manager
        .start(file_source(ids[0], &files[0], false))
        .await
        .unwrap();
    let b = manager
        .start(file_source(ids[1], &files[1], false))
        .await
        .unwrap();
    wait_runtime(&a, |state, _| state == RuntimeState::Stopped).await;
    wait_runtime(&b, |state, _| state == RuntimeState::Stopped).await;
    let mut config = live_config(&root);
    config.index_page_records = 1;
    config.maximum_total_index_bytes = 160;
    let first_provider = LiveRowProvider::new(config.clone()).unwrap();
    let second_provider = LiveRowProvider::new(config.clone()).unwrap();
    first_provider.register_source(a.clone()).unwrap();
    second_provider.register_source(b.clone()).unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            first_provider.drain_ready_updates(32);
            second_provider.drain_ready_updates(32);
            let states = [
                first_provider.source_status(ids[0]).unwrap().index,
                second_provider.source_status(ids[1]).unwrap().index,
            ];
            if states.contains(&IndexState::Limited) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("cross-provider budget race did not settle");
    assert!(derived_index_bytes(&root.path().join("derived")) <= 160);
    first_provider.shutdown().await;
    second_provider.shutdown().await;

    let restarted = LiveRowProvider::new(config).unwrap();
    restarted.register_source(a).unwrap();
    restarted.register_source(b).unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            restarted.drain_ready_updates(32);
            if ids.iter().all(|id| {
                restarted.source_status(*id).is_some_and(|status| {
                    matches!(status.index, IndexState::Ready | IndexState::Limited)
                })
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("restart did not reconcile existing indexes");
    assert!(derived_index_bytes(&root.path().join("derived")) <= 160);
    restarted.shutdown().await;
}

#[tokio::test]
async fn active_providers_must_share_one_cap_but_restart_can_change_it() {
    let root = TempDir::new().unwrap();
    let first_file = root.path().join("cap-a.log");
    let second_file = root.path().join("cap-b.log");
    fs::write(&first_file, b"a\n").unwrap();
    fs::write(&second_file, b"b\n").unwrap();
    let ids = [SourceId::new(), SourceId::new()];
    let manager = SourceManager::new(root.path().join("capture"), runtime_config()).unwrap();
    let first = manager
        .start(file_source(ids[0], &first_file, false))
        .await
        .unwrap();
    let second = manager
        .start(file_source(ids[1], &second_file, false))
        .await
        .unwrap();
    wait_runtime(&first, |state, _| state == RuntimeState::Stopped).await;
    wait_runtime(&second, |state, _| state == RuntimeState::Stopped).await;

    let mut small = live_config(&root);
    small.index_page_records = 1;
    small.maximum_total_index_bytes = 100;
    let owner = LiveRowProvider::new(small).unwrap();
    owner.register_source(first).unwrap();
    wait_index(&owner, ids[0], 1).await;

    let mut large = live_config(&root);
    large.index_page_records = 1;
    large.maximum_total_index_bytes = 200;
    let mismatched = LiveRowProvider::new(large.clone()).unwrap();
    mismatched.register_source(second.clone()).unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            mismatched.drain_ready_updates(16);
            if mismatched.source_status(ids[1]).unwrap().index == IndexState::Limited {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("active shared-cap mismatch was not refused");
    assert!(
        mismatched
            .source_status(ids[1])
            .unwrap()
            .last_error
            .unwrap()
            .contains("budget mismatch")
    );
    mismatched.shutdown().await;
    owner.shutdown().await;

    let restarted = LiveRowProvider::new(large).unwrap();
    restarted.register_source(second).unwrap();
    wait_index(&restarted, ids[1], 1).await;
    assert_eq!(derived_index_bytes(&root.path().join("derived")), 200);
    restarted.shutdown().await;
}

#[tokio::test]
async fn shared_cache_binds_indexes_to_distinct_capture_root_journals() {
    let root = TempDir::new().unwrap();
    let input = root.path().join("same-source.log");
    let source_id = SourceId::new();
    fs::write(&input, b"old-a\nold-b\n").unwrap();

    let first_manager =
        SourceManager::new(root.path().join("capture-a"), runtime_config()).unwrap();
    let first_handle = first_manager
        .start(file_source(source_id, &input, false))
        .await
        .unwrap();
    wait_runtime(&first_handle, |state, _| state == RuntimeState::Stopped).await;
    assert_eq!(first_handle.progress().generation, 1);

    let config = live_config(&root);
    let first_provider = LiveRowProvider::new(config.clone()).unwrap();
    first_provider.register_source(first_handle).unwrap();
    first_provider
        .register_raw_view("first", vec![source_id])
        .unwrap();
    wait_index(&first_provider, source_id, 2).await;
    assert_eq!(
        wait_page(&first_provider, "first", 0, 2).await[1].text,
        "old-b"
    );
    let first_artifact = first_provider.index_path(source_id);

    fs::write(&input, b"new-only-and-longer\n").unwrap();
    let second_manager =
        SourceManager::new(root.path().join("capture-b"), runtime_config()).unwrap();
    let second_handle = second_manager
        .start(file_source(source_id, &input, false))
        .await
        .unwrap();
    wait_runtime(&second_handle, |state, _| state == RuntimeState::Stopped).await;
    assert_eq!(second_handle.progress().generation, 1);

    // Keep the first provider alive: the second journal must neither reuse its
    // offsets nor collide with its active ownership lock.
    let second_provider = LiveRowProvider::new(config).unwrap();
    second_provider.register_source(second_handle).unwrap();
    second_provider
        .register_raw_view("second", vec![source_id])
        .unwrap();
    wait_index(&second_provider, source_id, 1).await;
    let rows = wait_page(&second_provider, "second", 0, 1).await;
    assert_eq!(rows[0].text, "new-only-and-longer");
    assert_eq!(
        second_provider
            .source_status(source_id)
            .unwrap()
            .indexed_records,
        1
    );
    let second_artifact = second_provider.index_path(source_id);
    assert_ne!(first_artifact, second_artifact);
    assert!(first_artifact.exists());
    assert!(second_artifact.exists());

    second_provider.shutdown().await;
    first_provider.shutdown().await;
}

/// A derived index held by someone else is a wait, not a death sentence.
///
/// The index is exclusively locked so two writers cannot corrupt it, and losing
/// that race is ordinary: the previous owner is finishing its shutdown. Before
/// this was bounded-retried, the worker reported `Error` and returned, so no row
/// request was ever served again and the pane stayed blank behind a confident
/// "query ready" for as long as the app ran.
#[tokio::test]
async fn a_momentarily_locked_index_is_waited_out_rather_than_abandoned() {
    let root = TempDir::new().unwrap();
    let input = root.path().join("contended.log");
    fs::write(&input, b"first\nsecond\nthird\n").unwrap();
    let id = SourceId::new();
    let manager = SourceManager::new(root.path().join("capture"), runtime_config()).unwrap();
    let handle = manager.start(file_source(id, &input, false)).await.unwrap();
    wait_runtime(&handle, |_, records| records >= 3).await;

    // Build the index once so its exact path exists, then let that owner go.
    let first = LiveRowProvider::new(live_config(&root)).unwrap();
    first.register_source(handle.clone()).unwrap();
    first.register_raw_view("raw", vec![id]).unwrap();
    wait_index(&first, id, 3).await;
    first.shutdown().await;

    let derived = root.path().join("derived");
    let artifact = fs::read_dir(&derived)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.to_string_lossy().ends_with(".rows.idx"))
        .expect("the first owner left a derived index behind");
    let holder = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&artifact)
        .unwrap();
    FileExt::try_lock_exclusive(&holder).expect("the released index can be taken");

    let second = LiveRowProvider::new(live_config(&root)).unwrap();
    second.register_source(handle.clone()).unwrap();
    second.register_raw_view("raw", vec![id]).unwrap();
    let before_contention = RowProvider::revision(&second, "raw");

    // While the lock is held the source says what it is waiting for, and keeps
    // saying it rather than settling into a failure.
    let contended = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            second.drain_ready_updates(64);
            if let Some(status) = second.source_status(id)
                && status.index == IndexState::Contended
            {
                break status;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("a held index must be reported as contended, not abandoned");
    assert!(
        contended
            .last_error
            .as_deref()
            .is_some_and(|reason| reason.contains("already owned")),
        "contention must name its cause: {:?}",
        contended.last_error
    );
    // A status change with no new record still has to reach the viewport. It is
    // what refills a view's bounded row-request budget, so a pane that gave up
    // while the index was held starts asking again once it is not.
    let contended_revision = RowProvider::revision(&second, "raw");
    assert_ne!(
        contended_revision, before_contention,
        "entering contention must ask the viewport to repaint"
    );
    tokio::time::sleep(Duration::from_millis(150)).await;
    second.drain_ready_updates(64);
    assert_eq!(
        second.source_status(id).unwrap().index,
        IndexState::Contended,
        "the worker must still be waiting, not terminated"
    );

    FileExt::unlock(&holder).unwrap();
    drop(holder);

    // Recovery needs no user action: rows arrive and the reason clears.
    let rows = wait_page(&second, "raw", 0, 3).await;
    assert_eq!(rows[0].text, "first");
    assert_eq!(rows[2].text, "third");
    let recovered = second.source_status(id).unwrap();
    assert!(
        matches!(recovered.index, IndexState::Ready | IndexState::Indexing),
        "recovered index state: {:?}",
        recovered.index
    );
    assert_eq!(
        recovered.last_error, None,
        "the contention reason must clear once the index opens"
    );
    assert_ne!(
        RowProvider::revision(&second, "raw"),
        contended_revision,
        "leaving contention must ask the viewport to repaint"
    );

    // A non-following file source stops on its own once it reaches the end.
    let _ = handle.stop().await;
    second.shutdown().await;
}

/// A conflict that never clears has to end as a reported failure, not an
/// unbounded wait.
#[tokio::test]
async fn an_index_held_past_the_retry_window_becomes_a_reported_failure() {
    let root = TempDir::new().unwrap();
    let input = root.path().join("held.log");
    fs::write(&input, b"only\n").unwrap();
    let id = SourceId::new();
    let manager = SourceManager::new(root.path().join("capture"), runtime_config()).unwrap();
    let handle = manager.start(file_source(id, &input, false)).await.unwrap();
    wait_runtime(&handle, |_, records| records >= 1).await;

    let first = LiveRowProvider::new(live_config(&root)).unwrap();
    first.register_source(handle.clone()).unwrap();
    first.register_raw_view("raw", vec![id]).unwrap();
    wait_index(&first, id, 1).await;
    first.shutdown().await;

    let artifact = fs::read_dir(root.path().join("derived"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.to_string_lossy().ends_with(".rows.idx"))
        .expect("derived index");
    let holder = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&artifact)
        .unwrap();
    FileExt::try_lock_exclusive(&holder).unwrap();

    let mut config = live_config(&root);
    config.index_lock_retry_window = Duration::from_millis(120);
    config.index_lock_retry_ceiling = Duration::from_millis(20);
    let second = LiveRowProvider::new(config).unwrap();
    second.register_source(handle.clone()).unwrap();
    second.register_raw_view("raw", vec![id]).unwrap();

    let failed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            second.drain_ready_updates(64);
            if let Some(status) = second.source_status(id)
                && status.index == IndexState::Error
            {
                break status;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("an index held past the window must be reported");
    let reason = failed.last_error.unwrap_or_default();
    assert!(
        reason.contains("already owned") && reason.contains("still held"),
        "the failure must say it waited and for how long: {reason}"
    );

    drop(holder);
    let _ = handle.stop().await;
    second.shutdown().await;
}

/// A cache too large to account for must not stop a new source showing rows.
///
/// Reconciliation of the shared index total is bounded, so a directory holding
/// more indexes than that bound leaves the total unknown. Unknown was treated as
/// exhausted: a brand-new private source, with nothing to do with any of those
/// files, was refused its own index and displayed nothing at all. Stale cache
/// entries are an ordinary consequence of running the product; they must degrade
/// the guarantee, not the viewer.
#[tokio::test]
async fn an_unaccountable_index_cache_still_serves_a_new_source_and_says_so() {
    let root = TempDir::new().unwrap();
    let input = root.path().join("fresh.log");
    fs::write(&input, b"alpha\nbeta\ngamma\n").unwrap();
    let derived = root.path().join("derived");
    fs::create_dir_all(&derived).unwrap();

    let mut config = live_config(&root);
    config.maximum_sources = 4;
    // `reconciliation_limit` is derived from the source bound and floored at 64,
    // so the directory has to be larger than that floor to leave the total
    // unaccountable — which is exactly what a day of accumulated runs produces.
    let unrelated = 96;
    for index in 0..unrelated {
        fs::write(
            derived.join(format!("{:032x}.stale.rows.idx", index)),
            b"stale index bytes",
        )
        .unwrap();
    }

    let id = SourceId::new();
    let manager = SourceManager::new(root.path().join("capture"), runtime_config()).unwrap();
    let handle = manager.start(file_source(id, &input, false)).await.unwrap();
    wait_runtime(&handle, |_, records| records >= 3).await;

    let provider = LiveRowProvider::new(config).unwrap();
    provider.register_source(handle.clone()).unwrap();
    provider.register_raw_view("raw", vec![id]).unwrap();

    let rows = wait_page(&provider, "raw", 0, 3).await;
    assert_eq!(rows[0].text, "alpha");
    assert_eq!(rows[2].text, "gamma");

    let status = provider.source_status(id).unwrap();
    assert_eq!(
        status.index,
        IndexState::BudgetUnverified,
        "an unaccountable total must be reported, not silently assumed fine"
    );
    let reason = status.last_error.unwrap_or_default();
    assert!(
        reason.contains("unverified"),
        "the reason must name what could not be confirmed: {reason}"
    );
    assert!(!reason.contains("refused"), "nothing was refused: {reason}");

    let _ = handle.stop().await;
    provider.shutdown().await;
}

/// A viewport that keeps moving must still be served.
///
/// Position requests describe the window the terminal was showing when it
/// asked. Served in order, a viewport that moves faster than rows arrive --
/// following a tail while capture is still running -- makes every window the
/// worker reaches one that has already scrolled away, and the queue fills with
/// stale windows until the request for what is actually on screen is dropped.
/// A 512 MB source drew nothing for fifteen minutes that way. Only the newest
/// window is served; the rest are reported superseded so their ranges stay
/// askable.
#[tokio::test]
async fn a_moving_viewport_is_served_its_newest_window_not_a_backlog() {
    let root = TempDir::new().unwrap();
    let input = root.path().join("moving.log");
    let fixture = (0..600)
        .map(|index| format!("row-{index:04}\n"))
        .collect::<String>();
    fs::write(&input, fixture).unwrap();
    let id = SourceId::new();
    let mut runtime = runtime_config();
    runtime.acquisition.partial_flush_interval = Duration::from_secs(10);
    let manager = SourceManager::new(root.path().join("capture"), runtime).unwrap();
    let handle = manager.start(file_source(id, &input, false)).await.unwrap();
    wait_runtime(&handle, |state, _| state == RuntimeState::Stopped).await;
    let mut config = live_config(&root);
    config.cache_rows = 32;
    let provider = LiveRowProvider::new(config).unwrap();
    provider.register_source(handle.clone()).unwrap();
    provider.register_raw_view("raw", vec![id]).unwrap();
    wait_index(&provider, id, 600).await;

    // Ask for a different window on every frame, the way following a growing
    // tail does, without draining replies in between.
    for start in 0..64 {
        let _ = provider.page(
            "raw",
            ViewportRequest {
                start: start * 8,
                len: 8,
            },
        );
    }
    // The window the user is actually on is the last one asked for.
    let final_window = ViewportRequest { start: 504, len: 8 };
    let rows = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            provider.drain_ready_updates(64);
            let page = provider.page("raw", final_window);
            if page.rows.len() == final_window.len {
                break page.rows;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the window the viewport settled on must be served");
    assert_eq!(rows[0].text, "row-0504");
    assert_eq!(rows[7].text, "row-0511");

    // The backlog was discarded rather than served one stale window at a time,
    // and discarding it left nothing pending that would block asking again.
    let stats = provider.stats();
    assert!(
        stats.superseded_requests > 0,
        "stale windows were served one at a time instead of skipped: {stats:?}"
    );

    // A range that was superseded is still askable: nothing is permanently
    // un-fetchable because an earlier request for it was dropped on the floor.
    let head = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            provider.drain_ready_updates(64);
            let page = provider.page("raw", ViewportRequest { start: 0, len: 8 });
            if page.rows.len() == 8 {
                break page.rows;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("a superseded range must still be askable");
    assert_eq!(head[0].text, "row-0000");

    provider.shutdown().await;
}
