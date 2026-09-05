use lvu::{
    QueryConstraints, QueryPurpose, QueryRequest, RowProvider, TextConstraint, ViewportRequest,
    terminal::QueryDispatcher,
};
use lvu_core::{
    Acquisition, CommandDefinition, CommandProgram, RestartPolicy, SourceDefinition, SourceId,
};
use lvu_ingest::{RuntimeConfig, RuntimeState, SourceHandle, SourceManager};
use lvu_live::{LiveConfig, LiveRowProvider};
use lvu_query::CompilerHostConfig;
use lvu_view::{NativeViewAdapter, ScanState, ViewConfig};
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write,
    sync::Arc,
    time::Duration,
};
use tempfile::TempDir;

fn source(id: SourceId, path: &std::path::Path, follow: bool) -> SourceDefinition {
    SourceDefinition {
        schema_version: 1,
        id,
        name: "view fixture".into(),
        acquisition: Acquisition::File {
            path: path.into(),
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

fn runtime_config() -> RuntimeConfig {
    let mut value = RuntimeConfig::default();
    value.acquisition.read_chunk_bytes = 64;
    value.acquisition.partial_flush_interval = Duration::from_millis(10);
    value.batch_records = 8;
    value.max_page_records = 16;
    value.max_page_bytes = 4096;
    value
}

fn configs(root: &TempDir) -> (LiveConfig, ViewConfig) {
    let mut live = LiveConfig::new(root.path().join("raw-index"));
    live.index_page_records = 8;
    live.index_page_bytes = 4096;
    live.cache_rows = 16;
    live.cache_bytes = 16 * 1024;
    let mut view = ViewConfig::new(root.path().join("view-index"));
    view.page_records = 4;
    view.page_bytes = 4096;
    view.compiler = Some(CompilerHostConfig {
        executable: "uv".into(),
        args: vec![
            "run".into(),
            "--project".into(),
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../python")
                .display()
                .to_string(),
            "--locked".into(),
            "python".into(),
            "-m".into(),
            "lvu_expr_helper".into(),
        ],
        request_limit: 64 * 1024,
        output_limit: 384 * 1024,
        stderr_limit: 32 * 1024,
        timeout: Duration::from_secs(10),
    });
    (live, view)
}

async fn wait_runtime(handle: &SourceHandle, records: u64) {
    let mut progress = handle.subscribe();
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            let current = progress.borrow().clone();
            if current.records >= records || current.state == RuntimeState::Stopped {
                break;
            }
            progress.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
}

async fn wait_completion(adapter: &mut NativeViewAdapter, revision: u64) -> lvu::QueryCompletion {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            adapter.drain_updates(64);
            if let Some(done) = adapter.poll()
                && done.revision == revision
            {
                break done;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap()
}

async fn wait_page(adapter: &mut NativeViewAdapter, len: usize) -> Vec<lvu::DisplayRow> {
    let rows = adapter.rows();
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            let page = rows.page("view", ViewportRequest { start: 0, len });
            adapter.drain_updates(64);
            if page.rows.len() == len {
                break page.rows;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap()
}

async fn wait_index(adapter: &mut NativeViewAdapter, id: &lvu::RowId) -> usize {
    let rows = adapter.rows();
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            if let Some(index) = rows.index_of_id("view", id) {
                break index;
            }
            adapter.drain_updates(64);
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap()
}

fn request(
    view: &str,
    generation: u64,
    revision: u64,
    base_revision: u64,
    text: Option<&str>,
    advanced: Option<&str>,
) -> QueryRequest {
    QueryRequest {
        view_id: view.into(),
        generation,
        revision,
        base_revision,
        base_constraints: QueryConstraints::default(),
        purpose: if advanced.is_some() {
            QueryPurpose::Advanced
        } else {
            QueryPurpose::Search
        },
        constraints: QueryConstraints {
            text: text.map(|literal| TextConstraint {
                literal: literal.into(),
                case_insensitive: true,
            }),
            advanced_polars: advanced.map(str::to_owned),
        },
    }
}

fn with_base(
    mut request: QueryRequest,
    text: Option<&str>,
    advanced: Option<&str>,
) -> QueryRequest {
    request.base_constraints = QueryConstraints {
        text: text.map(|literal| TextConstraint {
            literal: literal.into(),
            case_insensitive: true,
        }),
        advanced_polars: advanced.map(str::to_owned),
    };
    request
}

async fn setup(
    root: &TempDir,
    contents: &str,
    follow: bool,
) -> (SourceManager, SourceHandle, NativeViewAdapter) {
    let input = root.path().join("input.log");
    fs::write(&input, contents).unwrap();
    let manager = SourceManager::new(root.path().join("capture"), runtime_config()).unwrap();
    let handle = manager
        .start(source(SourceId::new(), &input, follow))
        .await
        .unwrap();
    wait_runtime(&handle, contents.lines().count() as u64).await;
    let (live_config, view_config) = configs(root);
    let raw = Arc::new(LiveRowProvider::new(live_config).unwrap());
    let adapter = NativeViewAdapter::new(raw, view_config).unwrap();
    adapter.register_source(handle.clone()).unwrap();
    adapter
        .register_view("view", vec![handle.source_id()])
        .unwrap();
    (manager, handle, adapter)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn literal_unicode_punctuation_clear_and_advanced_failure_preserve_view() {
    let root = TempDir::new().unwrap();
    let (manager, handle, mut adapter) = setup(
        &root,
        "plain\nError CAFÉ [x].\n{\"status\":500,\"message\":\"CAFÉ [x].\"}\n",
        false,
    )
    .await;

    assert_eq!(adapter.status("view").unwrap().state, ScanState::Raw);
    adapter
        .submit(request("view", 1, 1, 0, Some("cAfÉ [X]."), None))
        .unwrap();
    assert!(wait_completion(&mut adapter, 1).await.result.is_ok());
    assert_eq!(adapter.status("view").unwrap().matched_records, 2);
    let displayed = wait_page(&mut adapter, 2).await;
    assert_eq!(
        displayed
            .iter()
            .map(|row| row.id.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert!(displayed[0].text.contains("CAFÉ [x]."));
    let second = lvu::RowId::new(handle.source_id().0.to_string(), 2);
    assert_eq!(wait_index(&mut adapter, &second).await, 1);

    adapter
        .submit(with_base(
            request(
                "view",
                2,
                2,
                1,
                Some("[x]."),
                Some("pl.col('status') >= 500"),
            ),
            Some("cAfÉ [X]."),
            None,
        ))
        .unwrap();
    assert_eq!(adapter.rows().index_of_id("view", &second), Some(1));
    assert!(wait_completion(&mut adapter, 2).await.result.is_ok());
    assert_eq!(adapter.status("view").unwrap().matched_records, 1);
    let applied_revision = adapter.revision("view");

    adapter
        .submit(with_base(
            request(
                "view",
                3,
                3,
                2,
                Some("plain"),
                Some("pl.col('status').sum() > 0"),
            ),
            Some("[x]."),
            Some("pl.col('status') >= 500"),
        ))
        .unwrap();
    let failed = wait_completion(&mut adapter, 3).await;
    assert!(failed.result.is_err());
    assert_eq!(adapter.revision("view"), applied_revision);
    assert_eq!(adapter.status("view").unwrap().matched_records, 1);

    adapter
        .submit(with_base(
            request("view", 4, 4, 2, None, None),
            Some("[x]."),
            Some("pl.col('status') >= 500"),
        ))
        .unwrap();
    assert!(wait_completion(&mut adapter, 4).await.result.is_ok());
    assert_eq!(adapter.status("view").unwrap().state, ScanState::Raw);
    assert_eq!(adapter.membership_bytes_used(), 0);
    adapter.shutdown();
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn latest_request_fences_scan_and_index_limit_is_explicit() {
    let root = TempDir::new().unwrap();
    let contents = (0..300)
        .map(|n| format!("row-{n} needle\n"))
        .collect::<String>();
    let (manager, _handle, mut adapter) = setup(&root, &contents, false).await;
    adapter
        .submit(request("view", 1, 1, 0, Some("needle"), None))
        .unwrap();
    adapter
        .submit(request("view", 2, 2, 0, Some("absent"), None))
        .unwrap();
    assert!(wait_completion(&mut adapter, 2).await.result.is_ok());
    assert_eq!(adapter.status("view").unwrap().matched_records, 0);
    adapter.shutdown();
    manager.shutdown().await;

    let root = TempDir::new().unwrap();
    let input = root.path().join("input.log");
    fs::write(&input, "yes\nyes\nyes\n").unwrap();
    let manager = SourceManager::new(root.path().join("capture"), runtime_config()).unwrap();
    let handle = manager
        .start(source(SourceId::new(), &input, false))
        .await
        .unwrap();
    wait_runtime(&handle, 3).await;
    let (live, mut view) = configs(&root);
    view.maximum_index_bytes = 280;
    let raw = Arc::new(LiveRowProvider::new(live).unwrap());
    let mut adapter = NativeViewAdapter::new(raw, view).unwrap();
    adapter.register_source(handle.clone()).unwrap();
    adapter
        .register_view("view", vec![handle.source_id()])
        .unwrap();
    adapter
        .submit(request("view", 1, 1, 0, Some("yes"), None))
        .unwrap();
    assert!(wait_completion(&mut adapter, 1).await.result.is_ok());
    assert_eq!(adapter.membership_bytes_used(), 152);
    adapter
        .submit(with_base(
            request("view", 2, 2, 1, Some("yes"), None),
            Some("yes"),
            None,
        ))
        .unwrap();
    assert!(wait_completion(&mut adapter, 2).await.result.is_err());
    assert_eq!(adapter.status("view").unwrap().state, ScanState::Limited);
    assert_eq!(adapter.membership_bytes_used(), 152);
    let rows = adapter.rows();
    assert_eq!(
        rows.page("view", ViewportRequest { start: 0, len: 1 })
            .total,
        3
    );
    adapter.shutdown();
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn command_arrivals_refresh_an_applied_native_query_and_shutdown_is_bounded() {
    let root = TempDir::new().unwrap();
    let manager = SourceManager::new(root.path().join("capture"), runtime_config()).unwrap();
    let handle = manager
        .start(command_source(
            SourceId::new(),
            "printf 'first hit\\n'; sleep .25; printf 'second HIT\\n'",
        ))
        .await
        .unwrap();
    wait_runtime(&handle, 1).await;
    let (live, view) = configs(&root);
    let raw = Arc::new(LiveRowProvider::new(live).unwrap());
    let mut adapter = NativeViewAdapter::new(raw, view).unwrap();
    adapter.register_source(handle.clone()).unwrap();
    adapter
        .register_view("view", vec![handle.source_id()])
        .unwrap();
    adapter
        .submit(request("view", 1, 1, 0, Some("hit"), None))
        .unwrap();
    assert!(wait_completion(&mut adapter, 1).await.result.is_ok());
    assert_eq!(adapter.status("view").unwrap().matched_records, 1);
    adapter
        .submit(with_base(
            request(
                "view",
                2,
                2,
                1,
                Some("hit"),
                Some("pl.col('missing').sum() > 0"),
            ),
            Some("hit"),
            None,
        ))
        .unwrap();
    assert!(wait_completion(&mut adapter, 2).await.result.is_err());
    assert_eq!(adapter.status("view").unwrap().state, ScanState::Ready);
    wait_runtime(&handle, 2).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            adapter.drain_updates(64);
            if adapter.status("view").unwrap().matched_records == 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(adapter.status("view").unwrap().scanned_records, 1);
    let displayed = wait_page(&mut adapter, 2).await;
    assert_eq!(
        displayed
            .iter()
            .map(|row| row.id.sequence)
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
    let start = std::time::Instant::now();
    adapter.shutdown();
    assert!(start.elapsed() < Duration::from_secs(1));
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_candidate_progress_does_not_advance_applied_high_watermark() {
    let root = TempDir::new().unwrap();
    let input = root.path().join("evolving.log");
    fs::write(&input, "keep zero\nkeep one\n").unwrap();
    let manager = SourceManager::new(root.path().join("capture"), runtime_config()).unwrap();
    let handle = manager
        .start(source(SourceId::new(), &input, true))
        .await
        .unwrap();
    wait_runtime(&handle, 2).await;
    let (live, mut view) = configs(&root);
    view.page_records = 1;
    let raw = Arc::new(LiveRowProvider::new(live).unwrap());
    let mut adapter = NativeViewAdapter::new(raw, view).unwrap();
    adapter.register_source(handle.clone()).unwrap();
    adapter
        .register_view("view", vec![handle.source_id()])
        .unwrap();
    adapter
        .submit(request("view", 1, 1, 0, Some("keep"), None))
        .unwrap();
    assert!(wait_completion(&mut adapter, 1).await.result.is_ok());

    let mut file = OpenOptions::new().append(true).open(&input).unwrap();
    for index in 0..100 {
        writeln!(file, "keep extra {index}").unwrap();
    }
    file.flush().unwrap();
    wait_runtime(&handle, 102).await;
    adapter
        .submit(with_base(
            request(
                "view",
                2,
                2,
                1,
                Some("keep"),
                Some("pl.col('raw').str.contains('keep')"),
            ),
            Some("keep"),
            None,
        ))
        .unwrap();

    // Let the candidate report at least one scanned page, then supersede it
    // with an invalid draft. Candidate progress must not become the applied
    // view's checkpoint.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            adapter.drain_updates(1);
            let status = adapter.status("view").unwrap();
            if status.state == ScanState::Pending && status.scanned_records != 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    adapter
        .submit(with_base(
            request("view", 3, 3, 1, Some("keep"), Some("pl.col('raw').sum()")),
            Some("keep"),
            None,
        ))
        .unwrap();
    assert!(wait_completion(&mut adapter, 3).await.result.is_err());

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            adapter.drain_updates(64);
            if adapter.status("view").unwrap().matched_records == 102 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let latest = lvu::RowId::new(handle.source_id().0.to_string(), 101);
    assert_eq!(adapter.rows().index_of_id("view", &latest), Some(101));

    writeln!(file, "keep final").unwrap();
    file.flush().unwrap();
    wait_runtime(&handle, 103).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            adapter.drain_updates(64);
            if adapter.status("view").unwrap().matched_records == 103 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let latest = lvu::RowId::new(handle.source_id().0.to_string(), 102);
    assert_eq!(adapter.rows().index_of_id("view", &latest), Some(102));
    adapter.shutdown();
    manager.shutdown().await;
}
