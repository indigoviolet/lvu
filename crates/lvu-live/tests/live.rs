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
    config.maximum_index_bytes_per_source = 124;
    config.cache_rows = 1;
    let provider = LiveRowProvider::new(config).unwrap();
    let artifact = provider.index_path(id);
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
    assert!(fs::metadata(artifact).unwrap().len() <= 124);
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
    let artifact = first.index_path(id);
    first.register_source(handle.clone()).unwrap();
    wait_index(&first, id, 4).await;
    first.shutdown().await;

    let complete_length = fs::metadata(&artifact).unwrap().len();
    assert_eq!(complete_length, 44 + 4 * 40);
    fs::OpenOptions::new()
        .write(true)
        .open(&artifact)
        .unwrap()
        .set_len(44 + 40)
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

    let contender = LiveRowProvider::new(config).unwrap();
    contender.register_source(handle).unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            contender.drain_ready_updates(8);
            if contender.source_status(id).unwrap().index == IndexState::Error {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("overlapping index owner was not rejected");
    assert!(
        contender
            .source_status(id)
            .unwrap()
            .last_error
            .unwrap()
            .contains("already owned")
    );
    assert_eq!(wait_page(&owner, "raw", 0, 1).await[0].text, "owned");
    contender.shutdown().await;
    owner.shutdown().await;
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
