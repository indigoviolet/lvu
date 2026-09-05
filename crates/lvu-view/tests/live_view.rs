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
use lvu_view::{NativeViewAdapter, ScanState, SnapshotLimits, SnapshotState, ViewConfig};
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write,
    sync::Arc,
    time::Duration,
};
use tempfile::TempDir;

fn wait_snapshot(job: &lvu_view::SnapshotJob) -> lvu_view::SnapshotStatus {
    let started = std::time::Instant::now();
    loop {
        let status = job.poll();
        if !matches!(
            status.state,
            SnapshotState::Pending | SnapshotState::Running
        ) {
            return status;
        }
        assert!(started.elapsed() < Duration::from_secs(15));
        std::thread::sleep(Duration::from_millis(5));
    }
}

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
            enrichment: None,
            capture_time: None,
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
        enrichment: None,
        capture_time: None,
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
    let raw = wait_page(&mut adapter, 3).await;
    let capture = raw[1].captured_at_unix_nanos.unwrap();
    let mut timed = request("view", 5, 5, 4, None, None);
    timed.constraints.capture_time = Some(lvu::CaptureTimeRange {
        start_unix_nanos: capture,
        end_unix_nanos: capture + 1,
    });
    adapter.submit(timed).unwrap();
    assert!(wait_completion(&mut adapter, 5).await.result.is_ok());
    let timed_rows = adapter
        .rows()
        .page("view", ViewportRequest { start: 0, len: 3 })
        .rows;
    assert!(
        timed_rows
            .iter()
            .all(|row| row.captured_at_unix_nanos == Some(capture))
    );
    assert!(!timed_rows.is_empty());
    adapter.shutdown();
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enrichment_projects_scalar_values_filters_arrivals_and_preserves_raw() {
    let root = TempDir::new().unwrap();
    let (manager, handle, mut adapter) = setup(
        &root,
        "malformed raw\nstatus=200 first\nstatus=503 failed\n",
        true,
    )
    .await;
    let expression = r#"status_code = pl.col("raw").str.extract(r"status=(\d+)", 1).cast(pl.Int64, strict=False)"#;
    let mut enrich = request("view", 1, 1, 0, None, None);
    enrich.purpose = QueryPurpose::Enrichment;
    enrich.constraints.enrichment = Some(expression.into());
    adapter.submit(enrich).unwrap();
    assert!(wait_completion(&mut adapter, 1).await.result.is_ok());
    let rows = wait_page(&mut adapter, 3).await;
    assert_eq!(rows[0].text, "malformed raw");
    assert!(
        rows[0]
            .fields
            .contains(&("status_code".into(), "null".into()))
    );
    assert!(
        rows[2]
            .fields
            .contains(&("status_code".into(), "503".into()))
    );

    let applied = QueryConstraints {
        enrichment: Some(expression.into()),
        capture_time: None,
        ..QueryConstraints::default()
    };
    let mut filtered = request("view", 2, 2, 1, None, Some("pl.col('status_code') >= 500"));
    filtered.base_constraints = applied.clone();
    filtered.constraints.enrichment = Some(expression.into());
    adapter.submit(filtered).unwrap();
    assert!(wait_completion(&mut adapter, 2).await.result.is_ok());
    let rows = wait_page(&mut adapter, 1).await;
    assert_eq!(rows[0].text, "status=503 failed");

    let input = root.path().join("input.log");
    let mut file = OpenOptions::new().append(true).open(input).unwrap();
    writeln!(file, "status=404 unmatched").unwrap();
    writeln!(file, "status=500 late").unwrap();
    file.flush().unwrap();
    wait_runtime(&handle, 5).await;
    let rows = wait_page(&mut adapter, 2).await;
    assert_eq!(rows[1].text, "status=500 late");

    let mut invalid = request("view", 3, 3, 2, None, Some("pl.col('status_code') >= 500"));
    invalid.purpose = QueryPurpose::Enrichment;
    invalid.base_constraints = QueryConstraints {
        advanced_polars: Some("pl.col('status_code') >= 500".into()),
        enrichment: Some(expression.into()),
        ..QueryConstraints::default()
    };
    invalid.constraints = invalid.base_constraints.clone();
    invalid.constraints.enrichment = Some("status_code = pl.col('missing_field')".into());
    adapter.submit(invalid).unwrap();
    let failed = wait_completion(&mut adapter, 3).await;
    assert!(failed.result.is_err());
    assert_eq!(failed.result.unwrap_err().purpose, QueryPurpose::Enrichment);
    assert_eq!(wait_page(&mut adapter, 2).await[1].text, "status=500 late");

    writeln!(file, "status=502 after rejected edit").unwrap();
    file.flush().unwrap();
    wait_runtime(&handle, 6).await;
    let rows = wait_page(&mut adapter, 3).await;
    assert_eq!(rows[2].text, "status=502 after rejected edit");
    assert!(
        rows[2]
            .fields
            .contains(&("status_code".into(), "502".into()))
    );
    let mut oversized = request("view", 4, 4, 2, None, Some("pl.col('status_code') >= 500"));
    oversized.purpose = QueryPurpose::Enrichment;
    oversized.base_constraints = QueryConstraints {
        advanced_polars: Some("pl.col('status_code') >= 500".into()),
        enrichment: Some(expression.into()),
        ..QueryConstraints::default()
    };
    oversized.constraints = oversized.base_constraints.clone();
    oversized.constraints.enrichment = Some(format!("{} = pl.lit(1)", "x".repeat(65)));
    adapter.submit(oversized).unwrap();
    assert!(wait_completion(&mut adapter, 4).await.result.is_err());
    adapter.shutdown();
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn accepted_enrichment_runtime_error_keeps_new_raw_row_with_diagnostic() {
    let root = TempDir::new().unwrap();
    let (manager, handle, mut adapter) = setup(&root, "status=200 initial\n", true).await;
    let expression = r#"status_code = pl.col("raw").str.extract(r"status=(\w+)", 1).cast(pl.Int64, strict=True)"#;
    let mut enrich = request("view", 1, 1, 0, None, None);
    enrich.purpose = QueryPurpose::Enrichment;
    enrich.constraints.enrichment = Some(expression.into());
    adapter.submit(enrich).unwrap();
    assert!(wait_completion(&mut adapter, 1).await.result.is_ok());

    let input = root.path().join("input.log");
    let mut file = OpenOptions::new().append(true).open(input).unwrap();
    writeln!(file, "status=bad still raw").unwrap();
    file.flush().unwrap();
    wait_runtime(&handle, 2).await;
    let rows = wait_page(&mut adapter, 2).await;
    assert_eq!(rows[1].text, "status=bad still raw");
    assert!(
        rows[1]
            .fields
            .iter()
            .any(|(name, value)| { name == "status_code" && value.starts_with("error:") })
    );
    assert!(
        adapter
            .status("view")
            .unwrap()
            .diagnostic
            .as_deref()
            .is_some_and(|message| message.contains("enrichment status_code failed"))
    );
    adapter.shutdown();
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dependent_filter_failure_never_admits_literal_nonmatches() {
    let root = TempDir::new().unwrap();
    let (manager, handle, mut adapter) = setup(
        &root,
        "request-123 status=500 initial\nunrelated status=500 initial\n",
        true,
    )
    .await;
    let expression = r#"status_code = pl.col("raw").str.extract(r"status=(\w+)", 1).cast(pl.Int64, strict=True)"#;
    let enrichment_only = QueryConstraints {
        enrichment: Some(expression.into()),
        ..QueryConstraints::default()
    };
    let mut enrich = request("view", 1, 1, 0, None, None);
    enrich.purpose = QueryPurpose::Enrichment;
    enrich.constraints = enrichment_only.clone();
    adapter.submit(enrich).unwrap();
    assert!(wait_completion(&mut adapter, 1).await.result.is_ok());

    let mut filtered = request(
        "view",
        2,
        2,
        1,
        Some("request-123"),
        Some("pl.col('status_code') >= 500"),
    );
    filtered.base_constraints = enrichment_only.clone();
    filtered.constraints.enrichment = Some(expression.into());
    adapter.submit(filtered).unwrap();
    assert!(wait_completion(&mut adapter, 2).await.result.is_ok());
    assert_eq!(
        wait_page(&mut adapter, 1).await[0].text,
        "request-123 status=500 initial"
    );

    let input = root.path().join("input.log");
    let mut file = OpenOptions::new().append(true).open(input).unwrap();
    writeln!(file, "request-123 status=bad matching raw").unwrap();
    writeln!(file, "unrelated status=bad must stay hidden").unwrap();
    file.flush().unwrap();
    wait_runtime(&handle, 4).await;
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            adapter.drain_updates(64);
            let status = adapter.status("view").unwrap();
            if status
                .diagnostic
                .as_deref()
                .is_some_and(|value| value.contains("dependent filter could not be evaluated"))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let still_filtered = adapter
        .rows()
        .page("view", ViewportRequest { start: 0, len: 8 });
    assert_eq!(still_filtered.rows.len(), 1);
    assert!(
        !still_filtered
            .rows
            .iter()
            .any(|row| row.text.contains("unrelated"))
    );

    let filtered_constraints = QueryConstraints {
        text: Some(TextConstraint {
            literal: "request-123".into(),
            case_insensitive: true,
        }),
        advanced_polars: Some("pl.col('status_code') >= 500".into()),
        enrichment: Some(expression.into()),
        capture_time: None,
    };
    let mut clear_advanced = request("view", 3, 3, 2, Some("request-123"), None);
    clear_advanced.base_constraints = filtered_constraints;
    clear_advanced.constraints.enrichment = Some(expression.into());
    adapter.submit(clear_advanced).unwrap();
    assert!(wait_completion(&mut adapter, 3).await.result.is_ok());
    let visible = wait_page(&mut adapter, 2).await;
    assert!(visible.iter().all(|row| row.text.contains("request-123")));
    assert!(
        visible[1]
            .fields
            .iter()
            .any(|(name, value)| name == "status_code" && value.starts_with("error:"))
    );
    assert!(adapter.status("view").unwrap().diagnostic.is_some());
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
    let mut runtime = runtime_config();
    // This regression is about query publication fencing, not partial-line
    // capture. A 10 ms partial flush can legitimately split the concurrently
    // appended fixture under parallel scheduler load and invalidate its exact
    // one-record-per-line ID assertions.
    runtime.acquisition.partial_flush_interval = Duration::from_secs(60);
    let manager = SourceManager::new(root.path().join("capture"), runtime).unwrap();
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
    let burst = (0..100)
        .map(|index| format!("keep extra {index}\n"))
        .collect::<String>();
    file.write_all(burst.as_bytes()).unwrap();
    file.flush().unwrap();
    wait_runtime(&handle, 102).await;
    let captured = handle.progress();
    assert_eq!(
        captured.records, 102,
        "fixture framing changed: {captured:?}"
    );
    assert_eq!(
        captured.high_watermark.map(|record| record.sequence),
        Some(101),
        "fixture must establish an exact fixed boundary"
    );
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn snapshot_exports_fixed_applied_enriched_rows_and_complete_source_parts() {
    use polars::prelude::{AnyValue, ParquetReader, SerReader};

    let root = TempDir::new().unwrap();
    let input = root.path().join("snapshot.log");
    fs::write(
        &input,
        b"{\"message\":\"drop\",\"value\":\"type conflict\"}\n{\"message\":\"keep!\",\"value\":2}\n{\"message\":\"KEEP?\",\"value\":3}\n",
    )
    .unwrap();
    let manager = SourceManager::new(root.path().join("capture"), runtime_config()).unwrap();
    let handle = manager
        .start(source(SourceId::new(), &input, true))
        .await
        .unwrap();
    wait_runtime(&handle, 3).await;
    let (live, mut view) = configs(&root);
    view.page_records = 3;
    let raw = Arc::new(LiveRowProvider::new(live).unwrap());
    let mut adapter = NativeViewAdapter::new(raw, view).unwrap();
    adapter.register_source(handle.clone()).unwrap();
    adapter
        .register_view("view", vec![handle.source_id()])
        .unwrap();
    let mut applied = request("view", 1, 1, 0, Some("keep"), None);
    applied.constraints.enrichment = Some("projected = pl.col('value')".into());
    applied.constraints.capture_time = Some(lvu::CaptureTimeRange {
        start_unix_nanos: i64::MIN,
        end_unix_nanos: i64::MAX,
    });
    adapter.submit(applied).unwrap();
    assert!(wait_completion(&mut adapter, 1).await.result.is_ok());
    let accepted_rows = wait_page(&mut adapter, 2).await;
    assert!(
        accepted_rows
            .iter()
            .all(|row| { row.fields.contains(&("projected".into(), "null".into())) })
    );

    let mut invalid = with_base(
        request(
            "view",
            99,
            2,
            1,
            Some("keep"),
            Some("pl.col('missing').sum()"),
        ),
        Some("keep"),
        None,
    );
    invalid.base_constraints.enrichment = Some("projected = pl.col('value')".into());
    invalid.base_constraints.capture_time = Some(lvu::CaptureTimeRange {
        start_unix_nanos: i64::MIN,
        end_unix_nanos: i64::MAX,
    });
    invalid.constraints.enrichment = Some("projected = pl.col('value')".into());
    invalid.constraints.capture_time = invalid.base_constraints.capture_time;
    adapter.submit(invalid).unwrap();

    let job = adapter
        .start_snapshot(
            "view",
            root.path().join("investigations"),
            SnapshotLimits {
                page_records: 1,
                page_bytes: 4096,
                ..SnapshotLimits::default()
            },
        )
        .unwrap();

    let mut file = OpenOptions::new().append(true).open(&input).unwrap();
    writeln!(file, "{{\"message\":\"keep later\",\"value\":4}}").unwrap();
    file.flush().unwrap();
    wait_runtime(&handle, 4).await;
    let status = wait_snapshot(&job);
    assert_eq!(status.state, SnapshotState::Complete);
    let manifest_path = status.manifest_path.unwrap();
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    assert_eq!(manifest["schema_version"], 1);
    assert_eq!(manifest["state"], "complete");
    assert_eq!(manifest["view"]["applied_revision"], 1);
    assert_eq!(manifest["view"]["applied_generation"], 1);
    assert_eq!(manifest["view"]["literal_search"], "keep");
    assert_eq!(manifest["view"]["capture_time_start_unix_nanos"], i64::MIN);
    assert_eq!(manifest["view"]["capture_time_end_unix_nanos"], i64::MAX);
    assert!(manifest["view"]["advanced_polars"].is_null());
    assert_eq!(
        manifest["view"]["enrichment"],
        "projected = pl.col('value')"
    );
    assert!(manifest["view"]["compatibility_id"].is_string());
    assert_eq!(manifest["source_rows"], 3);
    assert_eq!(manifest["filtered_rows"], 2);
    assert_eq!(manifest["sources"][0]["high_watermark"], 2);

    let mut filtered_sequences = Vec::new();
    let mut filtered_sources = Vec::new();
    let mut filtered_raw = Vec::new();
    let mut all_projected_null = true;
    for part in manifest["filtered_parts"].as_array().unwrap() {
        let path = job.output_dir().join(part["path"].as_str().unwrap());
        let frame = ParquetReader::new(fs::File::open(path).unwrap())
            .finish()
            .unwrap();
        for row in 0..frame.height() {
            filtered_sources.push(
                frame
                    .column("_lvu_source_id")
                    .unwrap()
                    .get(row)
                    .unwrap()
                    .get_str()
                    .unwrap()
                    .to_owned(),
            );
            filtered_sequences.push(
                frame
                    .column("_lvu_sequence")
                    .unwrap()
                    .get(row)
                    .unwrap()
                    .try_extract::<u64>()
                    .unwrap(),
            );
            match frame.column("_lvu_raw_bytes").unwrap().get(row).unwrap() {
                AnyValue::Binary(bytes) => filtered_raw.push(bytes.to_vec()),
                AnyValue::BinaryOwned(bytes) => filtered_raw.push(bytes),
                value => panic!("expected binary raw bytes, got {value:?}"),
            }
            all_projected_null &=
                frame.column("projected").unwrap().get(row).unwrap() == AnyValue::Null;
            assert!(
                frame
                    .column("_lvu_captured_at_unix_nanos")
                    .unwrap()
                    .get(row)
                    .unwrap()
                    .try_extract::<i64>()
                    .is_ok()
            );
        }
    }
    assert_eq!(filtered_sources, vec![handle.source_id().0.to_string(); 2]);
    assert_eq!(filtered_sequences, vec![1, 2]);
    assert_eq!(
        filtered_raw,
        vec![
            br#"{"message":"keep!","value":2}"#.to_vec(),
            br#"{"message":"KEEP?","value":3}"#.to_vec(),
        ]
    );
    assert!(all_projected_null);

    let source_rows: usize = manifest["source_parts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|part| part["rows"].as_u64().unwrap() as usize)
        .sum();
    assert_eq!(source_rows, 3);
    assert!(wait_completion(&mut adapter, 2).await.result.is_err());
    adapter.shutdown();
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn snapshot_cancel_and_limits_never_publish_complete_manifest() {
    let root = TempDir::new().unwrap();
    let input = root.path().join("large.log");
    let data = (0..512)
        .map(|index| format!("row {index}\n"))
        .collect::<String>();
    fs::write(&input, data).unwrap();
    let manager = SourceManager::new(root.path().join("capture"), runtime_config()).unwrap();
    let handle = manager
        .start(source(SourceId::new(), &input, false))
        .await
        .unwrap();
    wait_runtime(&handle, 512).await;
    let (live, view) = configs(&root);
    let raw = Arc::new(LiveRowProvider::new(live).unwrap());
    let adapter = NativeViewAdapter::new(raw, view).unwrap();
    adapter.register_source(handle.clone()).unwrap();
    adapter
        .register_view("view", vec![handle.source_id()])
        .unwrap();

    let cancelled = adapter
        .start_snapshot(
            "view",
            root.path().join("cancelled"),
            SnapshotLimits {
                page_records: 1,
                page_bytes: 64,
                ..SnapshotLimits::default()
            },
        )
        .unwrap();
    cancelled.cancel();
    let cancelled_status = wait_snapshot(&cancelled);
    assert_eq!(cancelled_status.state, SnapshotState::Cancelled);
    assert!(!cancelled.output_dir().join("manifest.json").exists());

    let limited = adapter
        .start_snapshot(
            "view",
            root.path().join("limited"),
            SnapshotLimits {
                page_records: 8,
                page_bytes: 4096,
                maximum_rows: 2,
                ..SnapshotLimits::default()
            },
        )
        .unwrap();
    let limited_status = wait_snapshot(&limited);
    assert_eq!(limited_status.state, SnapshotState::Limited);
    assert!(!limited.output_dir().join("manifest.json").exists());

    let disk_limited = adapter
        .start_snapshot(
            "view",
            root.path().join("disk-limited"),
            SnapshotLimits {
                page_records: 8,
                page_bytes: 4096,
                maximum_disk_bytes: 1,
                ..SnapshotLimits::default()
            },
        )
        .unwrap();
    let disk_status = wait_snapshot(&disk_limited);
    assert_eq!(disk_status.state, SnapshotState::Limited);
    assert!(!disk_limited.output_dir().join("manifest.json").exists());

    let invalid_root = root.path().join("not-a-directory");
    fs::write(&invalid_root, b"occupied").unwrap();
    let failed = adapter
        .start_snapshot("view", &invalid_root, SnapshotLimits::default())
        .unwrap();
    assert_eq!(wait_snapshot(&failed).state, SnapshotState::Failed);
    assert!(!failed.output_dir().join("manifest.json").exists());
    drop(cancelled);
    drop(limited);
    drop(disk_limited);
    drop(failed);
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn snapshot_never_completes_when_frozen_journal_boundary_is_missing() {
    let root = TempDir::new().unwrap();
    let input = root.path().join("boundary.log");
    fs::write(&input, b"one\ntwo\nthree\n").unwrap();
    let capture_root = root.path().join("capture");
    let manager = SourceManager::new(&capture_root, runtime_config()).unwrap();
    let handle = manager
        .start(source(SourceId::new(), &input, false))
        .await
        .unwrap();
    wait_runtime(&handle, 3).await;
    let (live, view) = configs(&root);
    let raw = Arc::new(LiveRowProvider::new(live).unwrap());
    let mut adapter = NativeViewAdapter::new(raw, view).unwrap();
    adapter.register_source(handle.clone()).unwrap();
    adapter
        .register_view("view", vec![handle.source_id()])
        .unwrap();
    adapter
        .submit(request("view", 1, 1, 0, Some("e"), None))
        .unwrap();
    assert!(wait_completion(&mut adapter, 1).await.result.is_ok());

    let journal = capture_root
        .join(handle.source_id().0.to_string())
        .join("capture.journal");
    let boundary = lvu_core::JournalReader::open(&journal, handle.source_id())
        .unwrap()
        .read_page(0, 2, 4096)
        .unwrap()
        .next_offset;
    fs::OpenOptions::new()
        .write(true)
        .open(&journal)
        .unwrap()
        .set_len(boundary)
        .unwrap();

    let job = adapter
        .start_snapshot(
            "view",
            root.path().join("missing-boundary"),
            SnapshotLimits::default(),
        )
        .unwrap();
    let status = wait_snapshot(&job);
    assert_eq!(status.state, SnapshotState::Failed);
    assert!(!job.output_dir().join("manifest.json").exists());
    adapter.shutdown();
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn snapshot_rejects_restarted_source_generation_for_applied_membership() {
    let root = TempDir::new().unwrap();
    let input = root.path().join("restart.log");
    fs::write(&input, b"keep one\nkeep two\n").unwrap();
    let capture_root = root.path().join("capture");
    let source_id = SourceId::new();
    let manager = SourceManager::new(&capture_root, runtime_config()).unwrap();
    let original = manager
        .start(source(source_id, &input, false))
        .await
        .unwrap();
    wait_runtime(&original, 2).await;
    let (live, view) = configs(&root);
    let raw = Arc::new(LiveRowProvider::new(live).unwrap());
    let mut adapter = NativeViewAdapter::new(raw, view).unwrap();
    adapter.register_source(original).unwrap();
    adapter.register_view("view", vec![source_id]).unwrap();
    adapter
        .submit(request("view", 1, 1, 0, Some("keep"), None))
        .unwrap();
    assert!(wait_completion(&mut adapter, 1).await.result.is_ok());
    manager.shutdown().await;

    let restarted_manager = SourceManager::new(&capture_root, runtime_config()).unwrap();
    let restarted = restarted_manager
        .start(source(source_id, &input, false))
        .await
        .unwrap();
    assert!(restarted.progress().generation > 1);
    adapter.register_source(restarted).unwrap();
    let job = adapter
        .start_snapshot(
            "view",
            root.path().join("restarted"),
            SnapshotLimits::default(),
        )
        .unwrap();
    let status = wait_snapshot(&job);
    assert_eq!(status.state, SnapshotState::Failed);
    assert!(
        status
            .diagnostic
            .as_deref()
            .is_some_and(|message| message.contains("generation changed"))
    );
    assert!(!job.output_dir().join("manifest.json").exists());
    adapter.shutdown();
    restarted_manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn snapshot_preserves_incremental_enrichment_batch_boundaries() {
    use polars::prelude::{ParquetReader, SerReader};

    let root = TempDir::new().unwrap();
    let input = root.path().join("incremental.log");
    fs::write(&input, b"{\"status\":\"200\"}\n").unwrap();
    let manager = SourceManager::new(root.path().join("capture"), runtime_config()).unwrap();
    let handle = manager
        .start(source(SourceId::new(), &input, true))
        .await
        .unwrap();
    wait_runtime(&handle, 1).await;
    let (live, mut view) = configs(&root);
    view.page_records = 4;
    let raw = Arc::new(LiveRowProvider::new(live).unwrap());
    let mut adapter = NativeViewAdapter::new(raw, view).unwrap();
    adapter.register_source(handle.clone()).unwrap();
    adapter
        .register_view("view", vec![handle.source_id()])
        .unwrap();
    let expression = "code = pl.col('status').cast(pl.Int64, strict=True)";
    let mut applied = request("view", 1, 1, 0, None, None);
    applied.constraints.enrichment = Some(expression.into());
    adapter.submit(applied).unwrap();
    assert!(wait_completion(&mut adapter, 1).await.result.is_ok());
    assert!(
        wait_page(&mut adapter, 1).await[0]
            .fields
            .contains(&("code".into(), "200".into()))
    );

    let mut file = OpenOptions::new().append(true).open(&input).unwrap();
    writeln!(file, "{{\"status\":\"bad\"}}").unwrap();
    file.flush().unwrap();
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
    let live_rows = wait_page(&mut adapter, 2).await;
    assert!(live_rows[0].fields.contains(&("code".into(), "200".into())));
    assert!(
        live_rows[1]
            .fields
            .iter()
            .any(|(name, value)| name == "code" && value.starts_with("error:"))
    );

    let job = adapter
        .start_snapshot(
            "view",
            root.path().join("incremental-snapshot"),
            SnapshotLimits {
                page_records: 2,
                page_bytes: 4096,
                ..SnapshotLimits::default()
            },
        )
        .unwrap();
    let status = wait_snapshot(&job);
    assert_eq!(status.state, SnapshotState::Complete);
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(status.manifest_path.unwrap()).unwrap()).unwrap();
    assert_eq!(manifest["filtered_rows"], 2);
    let parts = manifest["filtered_parts"].as_array().unwrap();
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0]["enrichment_state"], "ready");
    assert_eq!(parts[1]["enrichment_state"], "error");
    let first = ParquetReader::new(
        fs::File::open(job.output_dir().join(parts[0]["path"].as_str().unwrap())).unwrap(),
    )
    .finish()
    .unwrap();
    assert_eq!(
        first
            .column("code")
            .unwrap()
            .get(0)
            .unwrap()
            .try_extract::<i64>()
            .unwrap(),
        200
    );
    adapter.shutdown();
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn snapshot_replays_batches_across_durable_sequence_reservation_gaps() {
    let root = TempDir::new().unwrap();
    let (manager, original, mut adapter) = setup(&root, "first\nsecond\n", false).await;
    let _ = original.stop().await;
    adapter.shutdown();
    let input = root.path().join("input.log");
    let mut file = OpenOptions::new().append(true).open(&input).unwrap();
    for number in 0..6 {
        writeln!(file, "later {number}").unwrap();
    }
    file.flush().unwrap();
    let handle = manager
        .start(source(original.source_id(), &input, false))
        .await
        .unwrap();
    wait_runtime(&handle, 8).await;
    let _ = handle.stop().await;
    assert!(handle.progress().high_watermark.unwrap().sequence > 8);
    let (live, view) = configs(&root);
    let raw = Arc::new(LiveRowProvider::new(live).unwrap());
    let mut adapter = NativeViewAdapter::new(raw, view).unwrap();
    adapter.register_source(handle.clone()).unwrap();
    adapter
        .register_view("view", vec![handle.source_id()])
        .unwrap();
    let mut query = request("view", 1, 1, 0, None, None);
    query.purpose = QueryPurpose::Enrichment;
    query.constraints.enrichment = Some("copy = pl.col('raw')".into());
    adapter.submit(query).unwrap();
    assert!(wait_completion(&mut adapter, 1).await.result.is_ok());
    let job = adapter
        .start_snapshot(
            "view",
            root.path().join("gap-export"),
            SnapshotLimits::default(),
        )
        .unwrap();
    let status = wait_snapshot(&job);
    assert_eq!(
        status.state,
        SnapshotState::Complete,
        "{:?}",
        status.diagnostic
    );
    assert_eq!(status.filtered_rows_written, 8);
    adapter.shutdown();
    manager.shutdown().await;
}
