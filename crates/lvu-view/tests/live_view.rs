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
use lvu_view::{
    FrozenInputError, FrozenInputLimits, NativeViewAdapter, ScanState, SnapshotLimits,
    SnapshotState, ViewConfig,
};
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write,
    sync::{Arc, atomic::AtomicBool},
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

/// A capture that frames only on line boundaries.
///
/// The shared config flushes a partial line every 10ms, which is real product
/// behaviour — raw bytes are shown before their line terminates — and it means a
/// 512-line file captures 513 or 514 records, the extra ones being fragments
/// with no parsed fields. A test whose subject is how a snapshot packs batches
/// and schemas should not also be sampling that race: the fragment's position
/// among the parts decides whether it passes. Pushing the interval past the
/// duration of a bounded file read makes the record count exact.
fn line_framed_runtime_config() -> RuntimeConfig {
    let mut value = runtime_config();
    value.acquisition.partial_flush_interval = Duration::from_secs(3600);
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
            enrichments: Vec::new(),
            ..QueryConstraints::default()
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
        enrichments: Vec::new(),
        ..QueryConstraints::default()
    };
    request
}

fn enrichment(source: impl Into<String>) -> Vec<lvu::EnrichmentDefinition> {
    vec![lvu::EnrichmentDefinition {
        id: lvu::EnrichmentStageId("legacy-enrichment".into()),
        source: source.into(),
        command: None,
    }]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn broad_row_local_chain_retains_alignment_and_live_refresh_after_neighbor_rejection() {
    let root = TempDir::new().unwrap();
    let (manager, handle, mut adapter) = setup(
        &root,
        "{\"message\":\"  alpha  \"}\n{\"message\":\" beta \"}\n",
        true,
    )
    .await;
    let mut chain = enrichment("clean = pl.col('message').str.strip_chars().str.to_uppercase()");
    chain.push(lvu::EnrichmentDefinition {
        id: lvu::EnrichmentStageId("dependent-prefix".into()),
        source: "prefix = pl.col('clean').str.slice(0, 2)".into(),
        command: None,
    });
    let mut accepted = request(
        "view",
        1,
        1,
        0,
        None,
        Some("pl.col('prefix').is_not_null()"),
    );
    accepted.purpose = QueryPurpose::Enrichment;
    accepted.constraints.enrichments = chain.clone();
    let accepted_constraints = accepted.constraints.clone();
    adapter.submit(accepted).unwrap();
    assert!(wait_completion(&mut adapter, 1).await.result.is_ok());
    let before = wait_page(&mut adapter, 2).await;
    assert!(before[0].fields.contains(&("clean".into(), "ALPHA".into())));
    assert!(before[1].fields.contains(&("prefix".into(), "BE".into())));

    let mut file = OpenOptions::new()
        .append(true)
        .open(root.path().join("input.log"))
        .unwrap();
    for (index, unsafe_source) in [
        "clean = pl.col('message').shift(1)",
        "clean = pl.col('message').reverse()",
        "clean = pl.col('message').fill_null(strategy='forward')",
    ]
    .into_iter()
    .enumerate()
    {
        let revision = index as u64 + 2;
        let mut rejected = request("view", revision, revision, 1, None, None);
        rejected.purpose = QueryPurpose::Enrichment;
        rejected.base_constraints = accepted_constraints.clone();
        rejected.constraints = accepted_constraints.clone();
        rejected.constraints.enrichments[0].source = unsafe_source.into();
        adapter.submit(rejected).unwrap();
        assert!(
            wait_completion(&mut adapter, revision)
                .await
                .result
                .is_err(),
            "{unsafe_source}"
        );
        writeln!(file, "{{\"message\":\" gamma{index} \"}}").unwrap();
        file.flush().unwrap();
        wait_runtime(&handle, index as u64 + 3).await;
        let rows = wait_page(&mut adapter, index + 3).await;
        assert_eq!(rows[0].id, before[0].id);
        assert_eq!(rows[1].id, before[1].id);
        let latest = rows.last().unwrap();
        assert!(
            latest
                .fields
                .contains(&("clean".into(), format!("GAMMA{index}")))
        );
        assert!(latest.fields.contains(&("prefix".into(), "GA".into())));
    }
    adapter.shutdown();
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_capture_never_accepts_neighbor_dependent_candidate() {
    let root = TempDir::new().unwrap();
    let (manager, handle, mut adapter) = setup(&root, "", true).await;
    let mut accepted = request("view", 1, 1, 0, None, None);
    accepted.purpose = QueryPurpose::Enrichment;
    accepted.constraints.enrichments = enrichment("label = pl.lit('accepted')");
    let accepted_constraints = accepted.constraints.clone();
    adapter.submit(accepted).unwrap();
    assert!(wait_completion(&mut adapter, 1).await.result.is_ok());
    let mut rejected = request("view", 2, 2, 1, None, None);
    rejected.purpose = QueryPurpose::Enrichment;
    rejected.base_constraints = accepted_constraints.clone();
    rejected.constraints = accepted_constraints;
    rejected.constraints.enrichments[0].source = "label = pl.col('raw').shift(1)".into();
    adapter.submit(rejected).unwrap();
    assert!(wait_completion(&mut adapter, 2).await.result.is_err());
    let mut file = OpenOptions::new()
        .append(true)
        .open(root.path().join("input.log"))
        .unwrap();
    writeln!(file, "arrived after rejected empty candidate").unwrap();
    file.flush().unwrap();
    wait_runtime(&handle, 1).await;
    let rows = wait_page(&mut adapter, 1).await;
    assert!(
        rows[0]
            .fields
            .contains(&("label".into(), "accepted".into()))
    );
    adapter.shutdown();
    manager.shutdown().await;
}

async fn setup(
    root: &TempDir,
    contents: &str,
    follow: bool,
) -> (SourceManager, SourceHandle, NativeViewAdapter) {
    setup_bytes(root, contents.as_bytes(), follow).await
}

async fn setup_bytes(
    root: &TempDir,
    contents: &[u8],
    follow: bool,
) -> (SourceManager, SourceHandle, NativeViewAdapter) {
    setup_bytes_with_runtime(root, contents, follow, runtime_config()).await
}

async fn setup_bytes_with_runtime(
    root: &TempDir,
    contents: &[u8],
    follow: bool,
    runtime: RuntimeConfig,
) -> (SourceManager, SourceHandle, NativeViewAdapter) {
    let input = root.path().join("input.log");
    fs::write(&input, contents).unwrap();
    let manager = SourceManager::new(root.path().join("capture"), runtime).unwrap();
    let handle = manager
        .start(source(SourceId::new(), &input, follow))
        .await
        .unwrap();
    wait_runtime(
        &handle,
        contents.iter().filter(|byte| **byte == b'\n').count() as u64,
    )
    .await;
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
    enrich.constraints.enrichments = enrichment(expression);
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
        enrichments: enrichment(expression),
        capture_time: None,
        time_basis: lvu::TimeBasis::Capture,
        grouping: None,
        ..QueryConstraints::default()
    };
    let mut filtered = request("view", 2, 2, 1, None, Some("pl.col('status_code') >= 500"));
    filtered.base_constraints = applied.clone();
    filtered.constraints.enrichments = enrichment(expression);
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
        enrichments: enrichment(expression),
        ..QueryConstraints::default()
    };
    invalid.constraints = invalid.base_constraints.clone();
    invalid.constraints.enrichments = enrichment("status_code = pl.col('missing_field')");
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
        enrichments: enrichment(expression),
        ..QueryConstraints::default()
    };
    oversized.constraints = oversized.base_constraints.clone();
    oversized.constraints.enrichments = enrichment(format!("{} = pl.lit(1)", "x".repeat(65)));
    adapter.submit(oversized).unwrap();
    assert!(wait_completion(&mut adapter, 4).await.result.is_err());
    adapter.shutdown();
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_regex_enrichment_adds_named_columns_filters_and_survives_invalid_edit() {
    use polars::prelude::{ParquetReader, SerReader};

    let root = TempDir::new().unwrap();
    let (manager, handle, mut adapter) = setup(
        &root,
        "request_id=a/1 status=200\nunmatched punctuation []{}!\nrequest_id=b status=503\n",
        true,
    )
    .await;
    let shorthand = r"/request_id=(?P<request_id>\S+).*status=(?P<status>\d+)/";
    let mut enrich = request("view", 1, 1, 0, None, None);
    enrich.purpose = QueryPurpose::Enrichment;
    enrich.constraints.enrichments = enrichment(shorthand);
    adapter.submit(enrich).unwrap();
    assert!(wait_completion(&mut adapter, 1).await.result.is_ok());
    assert_eq!(
        adapter.compiler_calls(),
        0,
        "regex shorthand is Python-free"
    );
    let rows = wait_page(&mut adapter, 3).await;
    assert_eq!(rows[0].text, "request_id=a/1 status=200");
    assert!(
        rows[0]
            .fields
            .contains(&("request_id".into(), "a/1".into()))
    );
    assert!(rows[0].fields.contains(&("status".into(), "200".into())));
    assert!(
        rows[1]
            .fields
            .contains(&("request_id".into(), "null".into()))
    );
    assert!(rows[1].fields.contains(&("status".into(), "null".into())));

    let mut filtered = request("view", 2, 2, 1, None, Some("pl.col('status') == '503'"));
    filtered.base_constraints.enrichments = enrichment(shorthand);
    filtered.constraints.enrichments = enrichment(shorthand);
    adapter.submit(filtered).unwrap();
    assert!(wait_completion(&mut adapter, 2).await.result.is_ok());
    assert_eq!(
        wait_page(&mut adapter, 1).await[0].text,
        "request_id=b status=503"
    );

    let mut invalid = request("view", 3, 3, 2, None, Some("pl.col('status') == '503'"));
    invalid.purpose = QueryPurpose::Enrichment;
    invalid.base_constraints.advanced_polars = Some("pl.col('status') == '503'".into());
    invalid.base_constraints.enrichments = enrichment(shorthand);
    invalid.constraints = invalid.base_constraints.clone();
    invalid.constraints.enrichments = enrichment("/(?P<raw>.*)/");
    adapter.submit(invalid).unwrap();
    assert!(wait_completion(&mut adapter, 3).await.result.is_err());

    let input = root.path().join("input.log");
    let mut file = OpenOptions::new().append(true).open(input).unwrap();
    writeln!(file, "request_id=c status=503").unwrap();
    file.flush().unwrap();
    wait_runtime(&handle, 4).await;
    let rows = wait_page(&mut adapter, 2).await;
    assert_eq!(rows[1].text, "request_id=c status=503");
    assert!(rows[1].fields.contains(&("request_id".into(), "c".into())));

    let snapshot = adapter
        .start_snapshot(
            "view",
            root.path().join("regex-snapshot"),
            SnapshotLimits {
                page_records: 1,
                page_bytes: 4096,
                ..SnapshotLimits::default()
            },
        )
        .unwrap();
    let status = wait_snapshot(&snapshot);
    assert_eq!(status.state, SnapshotState::Complete);
    let manifest: serde_json::Value = serde_json::from_slice(
        &fs::read(status.manifest_path.expect("complete manifest")).unwrap(),
    )
    .unwrap();
    assert_eq!(manifest["view"]["enrichments"][0]["source"], shorthand);
    let mut exported = Vec::new();
    for part in manifest["filtered_parts"].as_array().unwrap() {
        let frame = ParquetReader::new(
            fs::File::open(snapshot.output_dir().join(part["path"].as_str().unwrap())).unwrap(),
        )
        .finish()
        .unwrap();
        assert_eq!(
            frame.column("request_id").unwrap().dtype(),
            &polars::prelude::DataType::String
        );
        exported.extend(
            frame
                .column("request_id")
                .unwrap()
                .str()
                .unwrap()
                .iter()
                .flatten()
                .map(str::to_owned),
        );
        assert!(frame.column("status").is_ok());
    }
    assert_eq!(exported, ["b", "c"]);

    adapter.shutdown();
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ordered_typed_enrichment_additions_retain_prior_stages_and_remove_explicitly() {
    let root = TempDir::new().unwrap();
    let (manager, handle, mut adapter) = setup(
        &root,
        "request_id=a status=200\nrequest_id=b status=503\n",
        true,
    )
    .await;
    let extract = lvu::EnrichmentDefinition {
        id: lvu::EnrichmentStageId("extract-request".into()),
        source: r"/request_id=(?P<request_id>\S+).*status=(?P<status>\S+)/".into(),
        command: None,
    };
    let cast = lvu::EnrichmentDefinition {
        id: lvu::EnrichmentStageId("cast-status".into()),
        source: "status_num = pl.col('status').cast(pl.Int64, strict=True)".into(),
        command: None,
    };
    let independent = lvu::EnrichmentDefinition {
        id: lvu::EnrichmentStageId("extract-tag".into()),
        source: r"/request_id=(?P<tag>\S+)/".into(),
        command: None,
    };
    let active_filter = "pl.col('request_id').is_not_null()";

    let mut first = request("view", 1, 1, 0, None, None);
    first.purpose = QueryPurpose::Enrichment;
    first.constraints.enrichments = vec![extract.clone()];
    adapter.submit(first).unwrap();
    assert!(wait_completion(&mut adapter, 1).await.result.is_ok());

    let mut addition = request("view", 2, 2, 1, None, None);
    addition.purpose = QueryPurpose::Enrichment;
    addition.base_constraints.enrichments = vec![extract.clone()];
    addition.constraints.enrichments = vec![extract.clone(), cast.clone()];
    addition.constraints.advanced_polars = Some(active_filter.into());
    adapter.submit(addition).unwrap();
    assert!(wait_completion(&mut adapter, 2).await.result.is_ok());
    let rows = wait_page(&mut adapter, 2).await;
    assert!(rows[1].fields.contains(&("request_id".into(), "b".into())));
    assert!(
        rows[1]
            .fields
            .contains(&("status_num".into(), "503".into())),
        "fields: {:?}; status: {:?}",
        rows[1].fields,
        adapter.status("view")
    );

    let input = root.path().join("input.log");
    let mut file = OpenOptions::new().append(true).open(input).unwrap();
    writeln!(file, "request_id=bad status=oops").unwrap();
    file.flush().unwrap();
    wait_runtime(&handle, 3).await;
    let rows = wait_page(&mut adapter, 3).await;
    assert!(
        rows[2]
            .fields
            .iter()
            .any(|(name, value)| name == "status_num" && value.starts_with("error:"))
    );

    let mut independent_addition = request("view", 3, 3, 2, None, None);
    independent_addition.purpose = QueryPurpose::Enrichment;
    independent_addition.base_constraints.advanced_polars = Some(active_filter.into());
    independent_addition.constraints.advanced_polars = Some(active_filter.into());
    independent_addition.base_constraints.enrichments = vec![extract.clone(), cast.clone()];
    independent_addition.constraints.enrichments =
        vec![extract.clone(), cast.clone(), independent.clone()];
    adapter.submit(independent_addition).unwrap();
    assert!(wait_completion(&mut adapter, 3).await.result.is_ok());
    let rows = wait_page(&mut adapter, 3).await;
    assert!(rows[2].fields.contains(&("tag".into(), "bad".into())));
    assert!(
        rows[2]
            .fields
            .iter()
            .any(|(name, value)| name == "status_num" && value.starts_with("error:"))
    );

    let mut rejected = request("view", 4, 4, 3, None, None);
    rejected.purpose = QueryPurpose::Enrichment;
    rejected.base_constraints.advanced_polars = Some(active_filter.into());
    rejected.constraints.advanced_polars = Some(active_filter.into());
    rejected.base_constraints.enrichments =
        vec![extract.clone(), cast.clone(), independent.clone()];
    rejected.constraints.enrichments = vec![
        extract.clone(),
        cast.clone(),
        independent.clone(),
        lvu::EnrichmentDefinition {
            id: lvu::EnrichmentStageId("duplicate-output".into()),
            source: r"/(?P<status>.*)/".into(),
            command: None,
        },
    ];
    adapter.submit(rejected).unwrap();
    assert!(wait_completion(&mut adapter, 4).await.result.is_err());

    writeln!(file, "request_id=c status=501").unwrap();
    file.flush().unwrap();
    wait_runtime(&handle, 4).await;
    let rows = wait_page(&mut adapter, 4).await;
    assert!(rows[3].fields.contains(&("request_id".into(), "c".into())));
    assert!(
        rows[3]
            .fields
            .contains(&("status_num".into(), "501".into()))
    );
    assert!(rows[3].fields.contains(&("tag".into(), "c".into())));

    let mut invalid_dependency_removal = request("view", 5, 5, 3, None, None);
    invalid_dependency_removal.purpose = QueryPurpose::Enrichment;
    invalid_dependency_removal.base_constraints.advanced_polars = Some(active_filter.into());
    invalid_dependency_removal.constraints.advanced_polars = Some(active_filter.into());
    invalid_dependency_removal.base_constraints.enrichments =
        vec![extract.clone(), cast.clone(), independent.clone()];
    invalid_dependency_removal.constraints.enrichments = vec![cast.clone(), independent.clone()];
    adapter.submit(invalid_dependency_removal).unwrap();
    let removal_failure = wait_completion(&mut adapter, 5).await.result.unwrap_err();
    assert_eq!(removal_failure.purpose, QueryPurpose::Enrichment);
    assert!(
        wait_page(&mut adapter, 4).await[3]
            .fields
            .contains(&("request_id".into(), "c".into()))
    );

    let mut invalid_advanced = request("view", 6, 6, 3, None, None);
    invalid_advanced.purpose = QueryPurpose::Advanced;
    invalid_advanced.base_constraints.advanced_polars = Some(active_filter.into());
    invalid_advanced.constraints.advanced_polars = Some("pl.col(".into());
    invalid_advanced.base_constraints.enrichments =
        vec![extract.clone(), cast.clone(), independent.clone()];
    invalid_advanced.constraints.enrichments =
        vec![extract.clone(), cast.clone(), independent.clone()];
    adapter.submit(invalid_advanced).unwrap();
    let advanced_failure = wait_completion(&mut adapter, 6).await.result.unwrap_err();
    assert_eq!(advanced_failure.purpose, QueryPurpose::Advanced);

    let mut remove = request("view", 7, 7, 3, None, None);
    remove.purpose = QueryPurpose::Enrichment;
    remove.base_constraints.advanced_polars = Some(active_filter.into());
    remove.constraints.advanced_polars = Some(active_filter.into());
    remove.base_constraints.enrichments = vec![extract.clone(), cast, independent.clone()];
    remove.constraints.enrichments = vec![extract, independent];
    adapter.submit(remove).unwrap();
    assert!(wait_completion(&mut adapter, 7).await.result.is_ok());
    let rows = wait_page(&mut adapter, 4).await;
    assert!(
        rows.iter()
            .all(|row| row.fields.iter().all(|(name, _)| name != "status_num"))
    );
    assert!(
        rows.iter()
            .all(|row| row.fields.iter().any(|(name, _)| name == "request_id"))
    );

    adapter.shutdown();
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn time_only_revisions_reuse_compilation_expire_actual_rows_and_snapshot_exact_bounds() {
    let root = TempDir::new().unwrap();
    let (manager, _handle, mut adapter) = setup(&root, "first\nsecond\nthird\n", false).await;
    let raw = wait_page(&mut adapter, 3).await;
    let minimum = raw
        .iter()
        .filter_map(|row| row.captured_at_unix_nanos)
        .min()
        .unwrap();
    let maximum = raw
        .iter()
        .filter_map(|row| row.captured_at_unix_nanos)
        .max()
        .unwrap();
    let expression = "pl.col('raw').is_not_null()";
    let mut initial = request("view", 1, 1, 0, None, Some(expression));
    initial.constraints.capture_time = Some(lvu::CaptureTimeRange {
        start_unix_nanos: minimum.saturating_sub(1),
        end_unix_nanos: maximum.saturating_add(1),
    });
    adapter.submit(initial.clone()).unwrap();
    assert!(wait_completion(&mut adapter, 1).await.result.is_ok());
    assert_eq!(adapter.compiler_calls(), 1);
    assert_eq!(adapter.status("view").unwrap().matched_records, 3);

    let expired_range = lvu::CaptureTimeRange {
        start_unix_nanos: maximum.saturating_add(1),
        end_unix_nanos: maximum.saturating_add(2),
    };
    let mut expired = request("view", 2, 2, 1, None, Some(expression));
    expired.base_constraints = initial.constraints.clone();
    expired.constraints.capture_time = Some(expired_range);
    adapter.submit(expired.clone()).unwrap();
    assert!(wait_completion(&mut adapter, 2).await.result.is_ok());
    assert_eq!(
        adapter.compiler_calls(),
        1,
        "time-only refresh reuses Polars definitions"
    );
    assert_eq!(adapter.status("view").unwrap().matched_records, 0);
    assert!(
        adapter
            .rows()
            .page("view", ViewportRequest { start: 0, len: 8 })
            .rows
            .is_empty()
    );

    let job = adapter
        .start_snapshot(
            "view",
            root.path().join("rolling-snapshot"),
            SnapshotLimits::default(),
        )
        .unwrap();
    let status = wait_snapshot(&job);
    assert_eq!(status.state, SnapshotState::Complete);
    let manifest: serde_json::Value = serde_json::from_slice(
        &fs::read(status.manifest_path.expect("snapshot manifest")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        manifest["view"]["capture_time_start_unix_nanos"],
        expired_range.start_unix_nanos
    );
    assert_eq!(
        manifest["view"]["capture_time_end_unix_nanos"],
        expired_range.end_unix_nanos
    );
    assert_eq!(manifest["filtered_rows"], 0);
    adapter.shutdown();
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn event_time_filters_full_records_without_capture_fallback_and_exports_basis() {
    use polars::prelude::{ParquetReader, SerReader};
    let root = TempDir::new().unwrap();
    let input = concat!(
        "{\"message\":\"utc\",\"timestamp\":\"2026-09-05T12:30:45Z\"}\n",
        "time=2026-09-05T14:30:45+02:00 message=offset\n",
        "{\"message\":\"boundary\",\"ts\":\"2026-09-05T12:30:46Z\"}\n",
        "{\"message\":\"ambiguous\",\"ts\":\"2026-09-05 12:30:45\"}\n",
        "{\"message\":\"numeric\",\"ts\":1788611445}\n",
        "missing event time\n",
    );
    // The invalid/ambiguous and missing counts below are exact, so the capture
    // has to be exactly these lines and no partial-line fragment of them.
    let (manager, handle, mut adapter) =
        setup_bytes_with_runtime(&root, input.as_bytes(), true, line_framed_runtime_config()).await;
    let mut event = request("view", 1, 1, 0, None, None);
    event.constraints.time_basis = lvu::TimeBasis::Event;
    event.constraints.capture_time = Some(lvu::CaptureTimeRange {
        start_unix_nanos: 1_788_611_445_000_000_000,
        end_unix_nanos: 1_788_611_446_000_000_000,
    });
    adapter.submit(event).unwrap();
    assert!(wait_completion(&mut adapter, 1).await.result.is_ok());
    // Three of the six records are members. Native recognition reads the epoch
    // seconds record, so it now resolves to the same instant as the RFC3339 and
    // offset records and joins them inside the window; only the record with no
    // timezone stays out. The earlier expectation of two members and two
    // invalid records predates epoch support, and was never contradicted by a
    // two row viewport request, which is satisfied as soon as two rows exist.
    let rows = wait_page(&mut adapter, 3).await;
    assert_eq!(rows.len(), 3);
    assert!(rows[0].text.contains("utc"));
    assert!(rows[1].text.contains("offset"));
    assert!(rows[2].text.contains("numeric"));
    assert!(
        rows.iter()
            .all(|row| row.details.iter().any(|(key, value)| {
                key == "event_time_utc" && value == "2026-09-05T12:30:45.000000000Z"
            }))
    );
    // The unit is reported, not silently folded into the offset case.
    assert!(
        rows[2]
            .details
            .iter()
            .any(|(key, value)| { key == "event_time_note" && value.contains("epoch seconds") })
    );
    // `2026-09-05 12:30:45` carries no timezone. Read as UTC it would land
    // exactly inside this window, so its absence is what proves the zone is
    // still rejected rather than assumed. Membership and the diagnostic agree:
    // one invalid (no timezone) and one missing (no time at all).
    assert!(
        rows.iter().all(|row| !row.text.contains("ambiguous")),
        "a timezone-less event time must never be assumed into membership"
    );
    let status = adapter.status("view").unwrap();
    let diagnostic = status.diagnostic.unwrap();
    assert!(diagnostic.contains("1 invalid/ambiguous"), "{diagnostic}");
    assert!(diagnostic.contains("1 missing"), "{diagnostic}");

    let snapshot = adapter
        .start_snapshot(
            "view",
            root.path().join("event-snapshot"),
            SnapshotLimits::default(),
        )
        .unwrap();
    let status = wait_snapshot(&snapshot);
    assert_eq!(status.state, SnapshotState::Complete);
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(status.manifest_path.expect("manifest")).unwrap())
            .unwrap();
    assert_eq!(manifest["view"]["time_basis"], "event");
    assert_eq!(manifest["filtered_rows"], 3);
    let part = manifest["filtered_parts"][0]["path"].as_str().unwrap();
    let frame = ParquetReader::new(fs::File::open(snapshot.output_dir().join(part)).unwrap())
        .finish()
        .unwrap();
    assert!(frame.column("_lvu_event_time_unix_nanos").is_ok());
    let source_part = manifest["source_parts"][0]["path"].as_str().unwrap();
    let source_frame =
        ParquetReader::new(fs::File::open(snapshot.output_dir().join(source_part)).unwrap())
            .finish()
            .unwrap();
    // Only the timezone-less record and the record with no time at all are
    // null: the epoch record now exports a real instant.
    assert_eq!(
        source_frame
            .column("_lvu_event_time_unix_nanos")
            .unwrap()
            .null_count(),
        2
    );

    let mut file = OpenOptions::new()
        .append(true)
        .open(root.path().join("input.log"))
        .unwrap();
    writeln!(
        file,
        "{{\"message\":\"late event\",\"timestamp\":\"2026-09-05T12:30:45.5Z\"}}"
    )
    .unwrap();
    writeln!(file, "late raw without event time").unwrap();
    file.flush().unwrap();
    wait_runtime(&handle, 8).await;
    // The late RFC3339 event joins the three existing members; the late raw
    // line has no event time, so the missing count rises to two while the
    // timezone-less record stays the only invalid one.
    let late = wait_page(&mut adapter, 4).await;
    assert_eq!(late.len(), 4);
    assert!(late[3].text.contains("late event"));
    let diagnostic = adapter.status("view").unwrap().diagnostic.unwrap();
    assert!(diagnostic.contains("2 missing"), "{diagnostic}");
    assert!(diagnostic.contains("1 invalid/ambiguous"), "{diagnostic}");

    let mut expired = request("view", 2, 2, 1, None, None);
    expired.base_constraints.time_basis = lvu::TimeBasis::Event;
    expired.base_constraints.capture_time = Some(lvu::CaptureTimeRange {
        start_unix_nanos: 1_788_611_445_000_000_000,
        end_unix_nanos: 1_788_611_446_000_000_000,
    });
    expired.constraints.time_basis = lvu::TimeBasis::Event;
    expired.constraints.capture_time = Some(lvu::CaptureTimeRange {
        start_unix_nanos: 1_788_611_447_000_000_000,
        end_unix_nanos: 1_788_611_448_000_000_000,
    });
    adapter.submit(expired).unwrap();
    assert!(wait_completion(&mut adapter, 2).await.result.is_ok());
    assert_eq!(adapter.status("view").unwrap().matched_records, 0);
    adapter.shutdown();
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn display_grouping_preserves_physical_membership_orphans_bounds_and_snapshots() {
    let root = TempDir::new().unwrap();
    let (manager, handle, mut adapter) = setup(
        &root,
        "Error: boom\n  at first\n  at second\nnext event\n  at third\n",
        true,
    )
    .await;
    let rule = r"^(\s+|Caused by:)";
    let mut grouped = request("view", 1, 1, 0, None, None);
    grouped.purpose = QueryPurpose::Grouping;
    grouped.constraints.grouping = Some(rule.into());
    adapter.submit(grouped.clone()).unwrap();
    assert!(wait_completion(&mut adapter, 1).await.result.is_ok());
    let rows = wait_page(&mut adapter, 2).await;
    assert_eq!(rows.len(), 2);
    assert!(rows[0].text.contains("3 physical lines"), "{rows:#?}");
    assert!(rows[1].text.contains("2 physical lines"));
    assert_eq!(adapter.status("view").unwrap().matched_records, 5);

    let selected = rows[0].id.clone();
    let mut file = OpenOptions::new()
        .append(true)
        .open(root.path().join("input.log"))
        .unwrap();
    writeln!(file, "  at late").unwrap();
    file.flush().unwrap();
    wait_runtime(&handle, 6).await;
    let rows = tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            adapter.drain_updates(64);
            let rows = adapter
                .rows()
                .page("view", ViewportRequest { start: 0, len: 2 })
                .rows;
            if rows
                .get(1)
                .is_some_and(|row| row.text.contains("3 physical lines"))
            {
                break rows;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].id, selected);
    assert!(rows[1].text.contains("3 physical lines"));

    let mut only_frames = request("view", 2, 2, 1, Some("at"), None);
    only_frames.base_constraints = grouped.constraints.clone();
    only_frames.constraints.grouping = Some(rule.into());
    adapter.submit(only_frames.clone()).unwrap();
    assert!(wait_completion(&mut adapter, 2).await.result.is_ok());
    let orphaned = wait_page(&mut adapter, 2).await;
    assert_eq!(orphaned.len(), 2);
    assert!(
        orphaned
            .iter()
            .all(|row| row.text.contains("orphan continuation"))
    );

    let snapshot = adapter
        .start_snapshot(
            "view",
            root.path().join("grouped-snapshot"),
            SnapshotLimits::default(),
        )
        .unwrap();
    let status = wait_snapshot(&snapshot);
    assert_eq!(status.state, SnapshotState::Complete);
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(status.manifest_path.expect("manifest")).unwrap())
            .unwrap();
    assert_eq!(
        manifest["filtered_rows"], 4,
        "snapshot remains physical records"
    );

    let mut invalid = request("view", 3, 3, 2, Some("at"), None);
    invalid.purpose = QueryPurpose::Grouping;
    invalid.base_constraints = only_frames.constraints;
    invalid.constraints.text = invalid.base_constraints.text.clone();
    invalid.constraints.grouping = Some("(?=unsupported-lookaround)".into());
    adapter.submit(invalid).unwrap();
    let failed = wait_completion(&mut adapter, 3).await;
    assert_eq!(failed.result.unwrap_err().purpose, QueryPurpose::Grouping);
    assert_eq!(wait_page(&mut adapter, 2).await.len(), 2);
    adapter.shutdown();
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn display_grouping_splits_bounded_groups_and_snapshot_keeps_invalid_utf8_bytes() {
    use polars::prelude::{AnyValue, ParquetReader, SerReader};

    let root = TempDir::new().unwrap();
    let mut input = b"Error \xff\n".to_vec();
    for index in 0..65 {
        input.extend_from_slice(format!("  at frame-{index}\n").as_bytes());
    }
    // Group-size expectations require complete input lines. Read this fixed,
    // sub-4KiB fixture in one chunk so the separate 10ms partial-flush behavior
    // cannot split a physical line between tiny reads under scheduler load.
    let mut runtime = runtime_config();
    runtime.acquisition.read_chunk_bytes = 4096;
    assert!(input.len() < runtime.acquisition.read_chunk_bytes);
    let (manager, _handle, mut adapter) =
        setup_bytes_with_runtime(&root, &input, false, runtime).await;
    let mut grouped = request("view", 1, 1, 0, None, None);
    grouped.purpose = QueryPurpose::Grouping;
    grouped.constraints.grouping = Some(r"^\s+".into());
    adapter.submit(grouped).unwrap();
    assert!(wait_completion(&mut adapter, 1).await.result.is_ok());
    let rows = wait_page(&mut adapter, 2).await;
    assert!(rows[0].text.contains("64 physical lines"), "{rows:#?}");
    assert!(rows[1].text.contains("2 orphan continuation"));
    assert!(
        rows[1]
            .details
            .contains(&("group_overflow".into(), "bounded split".into()))
    );

    let snapshot = adapter
        .start_snapshot(
            "view",
            root.path().join("invalid-utf8-snapshot"),
            SnapshotLimits::default(),
        )
        .unwrap();
    let status = wait_snapshot(&snapshot);
    assert_eq!(status.state, SnapshotState::Complete);
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(status.manifest_path.unwrap()).unwrap()).unwrap();
    let first_part = manifest["filtered_parts"][0]["path"].as_str().unwrap();
    let frame = ParquetReader::new(fs::File::open(snapshot.output_dir().join(first_part)).unwrap())
        .finish()
        .unwrap();
    let first_raw = frame.column("_lvu_raw_bytes").unwrap().get(0).unwrap();
    assert!(matches!(first_raw, AnyValue::Binary(bytes) if bytes == b"Error \xff"));
    assert_eq!(manifest["filtered_rows"], 66);

    adapter.shutdown();
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn display_grouping_preserves_and_labels_one_oversized_physical_record() {
    let root = TempDir::new().unwrap();
    let mut input = vec![b'X'; 70 * 1024];
    input.push(b'\n');
    let input_path = root.path().join("input.log");
    fs::write(&input_path, &input).unwrap();
    let mut runtime = runtime_config();
    runtime.acquisition.read_chunk_bytes = 128 * 1024;
    runtime.acquisition.maximum_record_bytes = 128 * 1024;
    runtime.acquisition.partial_flush_interval = Duration::from_secs(60);
    runtime.max_page_bytes = 256 * 1024;
    let manager = SourceManager::new(root.path().join("capture"), runtime).unwrap();
    let handle = manager
        .start(source(SourceId::new(), &input_path, false))
        .await
        .unwrap();
    wait_runtime(&handle, 1).await;
    let (mut live_config, mut view_config) = configs(&root);
    live_config.index_page_bytes = 256 * 1024;
    live_config.cache_bytes = 256 * 1024;
    view_config.page_bytes = 256 * 1024;
    let raw = Arc::new(LiveRowProvider::new(live_config).unwrap());
    let mut adapter = NativeViewAdapter::new(raw, view_config).unwrap();
    adapter.register_source(handle.clone()).unwrap();
    adapter
        .register_view("view", vec![handle.source_id()])
        .unwrap();
    let mut grouped = request("view", 1, 1, 0, None, None);
    grouped.purpose = QueryPurpose::Grouping;
    grouped.constraints.grouping = Some("^X+$".into());
    adapter.submit(grouped).unwrap();
    assert!(wait_completion(&mut adapter, 1).await.result.is_ok());
    let row = wait_page(&mut adapter, 1).await.remove(0);
    assert!(row.text.contains("orphan continuation"));
    assert!(row.details.iter().any(|(key, value)| {
        key == "group_oversized_record"
            && value.contains("soft group limit")
            && value.contains("preserved alone")
    }));
    assert_eq!(adapter.status("view").unwrap().matched_records, 1);

    adapter.shutdown();
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn display_grouping_never_crosses_source_or_stream_boundaries() {
    let root = TempDir::new().unwrap();
    let first_path = root.path().join("first.log");
    let second_path = root.path().join("second.log");
    fs::write(&first_path, "header from first source\n").unwrap();
    fs::write(&second_path, "  continuation-shaped second source\n").unwrap();
    // `wait_runtime` waits for a record count, so a partial-line fragment can
    // satisfy it before the line it belongs to is framed. Shutdown then runs
    // mid-capture and reports the source incomplete.
    let manager =
        SourceManager::new(root.path().join("capture"), line_framed_runtime_config()).unwrap();
    let first = manager
        .start(source(SourceId::new(), &first_path, false))
        .await
        .unwrap();
    let second = manager
        .start(source(SourceId::new(), &second_path, false))
        .await
        .unwrap();
    let streams = manager
        .start(command_source(
            SourceId::new(),
            "printf 'command header\\n'; sleep 0.05; printf '  stderr continuation\\n' >&2",
        ))
        .await
        .unwrap();
    wait_runtime(&first, 1).await;
    wait_runtime(&second, 1).await;
    wait_runtime(&streams, 2).await;

    let (live_config, view_config) = configs(&root);
    let raw = Arc::new(LiveRowProvider::new(live_config).unwrap());
    let mut adapter = NativeViewAdapter::new(raw, view_config).unwrap();
    for handle in [&first, &second, &streams] {
        adapter.register_source(handle.clone()).unwrap();
    }
    adapter
        .register_view(
            "view",
            vec![first.source_id(), second.source_id(), streams.source_id()],
        )
        .unwrap();
    let mut grouped = request("view", 1, 1, 0, None, None);
    grouped.purpose = QueryPurpose::Grouping;
    grouped.constraints.grouping = Some(r"^\s+".into());
    adapter.submit(grouped).unwrap();
    assert!(wait_completion(&mut adapter, 1).await.result.is_ok());
    let rows = wait_page(&mut adapter, 4).await;
    assert_eq!(rows.len(), 4);
    assert!(rows.iter().all(|row| !row.text.contains("physical lines")));
    assert_eq!(
        rows.iter()
            .filter(|row| row.text.contains("orphan continuation"))
            .count(),
        2
    );

    adapter.shutdown();
    let report = manager.shutdown().await;
    assert!(
        report
            .iter()
            .all(|(_, result)| result.as_ref().is_ok_and(|item| item.complete))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn accepted_enrichment_runtime_error_keeps_new_raw_row_with_diagnostic() {
    let root = TempDir::new().unwrap();
    let (manager, handle, mut adapter) = setup(&root, "status=200 initial\n", true).await;
    let expression = r#"status_code = pl.col("raw").str.extract(r"status=(\w+)", 1).cast(pl.Int64, strict=True)"#;
    let mut enrich = request("view", 1, 1, 0, None, None);
    enrich.purpose = QueryPurpose::Enrichment;
    enrich.constraints.enrichments = enrichment(expression);
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
async fn event_time_counts_do_not_hide_enrichment_runtime_diagnostics() {
    let root = TempDir::new().unwrap();
    let initial = "status=200 timestamp=2026-09-05T12:30:45Z initial\n";
    let (manager, handle, mut adapter) = setup(&root, initial, true).await;
    let expression = r#"status_code = pl.col("raw").str.extract(r"status=(\w+)", 1).cast(pl.Int64, strict=True)"#;
    let mut applied = request("view", 1, 1, 0, None, None);
    applied.purpose = QueryPurpose::Enrichment;
    applied.constraints.enrichments = enrichment(expression);
    applied.constraints.time_basis = lvu::TimeBasis::Event;
    applied.constraints.capture_time = Some(lvu::CaptureTimeRange {
        start_unix_nanos: i64::MIN,
        end_unix_nanos: i64::MAX,
    });
    adapter.submit(applied.clone()).unwrap();
    assert!(wait_completion(&mut adapter, 1).await.result.is_ok());

    let mut file = OpenOptions::new()
        .append(true)
        .open(root.path().join("input.log"))
        .unwrap();
    writeln!(file, "status=bad missing event time").unwrap();
    file.flush().unwrap();
    wait_runtime(&handle, 2).await;
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            adapter.drain_updates(64);
            let diagnostic = adapter
                .status("view")
                .and_then(|status| status.diagnostic)
                .unwrap_or_default();
            if diagnostic.contains("enrichment status_code failed")
                && diagnostic.contains("event time: 1 missing")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("combined diagnostics");

    let mut clear_time = request("view", 2, 2, 1, None, None);
    clear_time.purpose = QueryPurpose::Enrichment;
    clear_time.base_constraints = applied.constraints;
    clear_time.constraints.enrichments = enrichment(expression);
    adapter.submit(clear_time).unwrap();
    assert!(wait_completion(&mut adapter, 2).await.result.is_ok());
    let rows = wait_page(&mut adapter, 2).await;
    assert_eq!(rows[1].text, "status=bad missing event time");
    assert!(
        rows[1]
            .fields
            .iter()
            .any(|(name, value)| { name == "status_code" && value.starts_with("error:") })
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
        enrichments: enrichment(expression),
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
    filtered.constraints.enrichments = enrichment(expression);
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
        enrichments: enrichment(expression),
        ..QueryConstraints::default()
    };
    let mut clear_advanced = request("view", 3, 3, 2, Some("request-123"), None);
    clear_advanced.base_constraints = filtered_constraints;
    clear_advanced.constraints.enrichments = enrichment(expression);
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
    assert_eq!(adapter.membership_bytes_used(), 176);
    adapter
        .submit(with_base(
            request("view", 2, 2, 1, Some("yes"), None),
            Some("yes"),
            None,
        ))
        .unwrap();
    assert!(wait_completion(&mut adapter, 2).await.result.is_err());
    assert_eq!(adapter.status("view").unwrap().state, ScanState::Limited);
    assert_eq!(adapter.membership_bytes_used(), 176);
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
    applied.constraints.enrichments = enrichment("projected = pl.col('value')");
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
    invalid.base_constraints.enrichments = enrichment("projected = pl.col('value')");
    invalid.base_constraints.capture_time = Some(lvu::CaptureTimeRange {
        start_unix_nanos: i64::MIN,
        end_unix_nanos: i64::MAX,
    });
    invalid.constraints.enrichments = enrichment("projected = pl.col('value')");
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
    assert_eq!(manifest["schema_version"], 2);
    let schemas = manifest["schemas"].as_array().unwrap();
    assert!(!schemas.is_empty());
    for part in manifest["source_parts"]
        .as_array()
        .unwrap()
        .iter()
        .chain(manifest["filtered_parts"].as_array().unwrap())
    {
        assert!(part.get("fields").is_none());
        let schema_id = part["schema_id"].as_u64().unwrap();
        assert!(schemas.iter().any(|schema| {
            schema["schema_id"].as_u64() == Some(schema_id) && schema["fields"].is_array()
        }));
    }
    assert_eq!(manifest["state"], "complete");
    assert_eq!(manifest["view"]["applied_revision"], 1);
    assert_eq!(manifest["view"]["applied_generation"], 1);
    assert_eq!(manifest["view"]["literal_search"], "keep");
    assert_eq!(manifest["view"]["capture_time_start_unix_nanos"], i64::MIN);
    assert_eq!(manifest["view"]["capture_time_end_unix_nanos"], i64::MAX);
    assert!(manifest["view"]["advanced_polars"].is_null());
    assert_eq!(
        manifest["view"]["enrichments"][0]["source"],
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
    let sample = &manifest["inspection_sample"];
    assert_eq!(sample["requested_rows"], 2);
    assert_eq!(sample["sources"][0]["dataset"], "applied_view");
    let mut sampled_sequences = Vec::new();
    for part in sample["sources"][0]["parts"].as_array().unwrap() {
        let frame = ParquetReader::new(
            fs::File::open(job.output_dir().join(part["path"].as_str().unwrap())).unwrap(),
        )
        .finish()
        .unwrap();
        assert!(
            frame.column("projected").is_ok(),
            "accepted derived columns are inspectable"
        );
        for offset in part["row_offsets"].as_array().unwrap() {
            sampled_sequences.push(
                frame
                    .column("_lvu_sequence")
                    .unwrap()
                    .get(offset.as_u64().unwrap() as usize)
                    .unwrap()
                    .try_extract::<u64>()
                    .unwrap(),
            );
        }
    }
    assert_eq!(sampled_sequences, vec![1, 2]);
    #[cfg(target_os = "linux")]
    assert_python_inspects_rust_snapshot(job.output_dir(), &manifest);
    assert!(wait_completion(&mut adapter, 2).await.result.is_err());
    adapter.shutdown();
    manager.shutdown().await;
}

#[cfg(target_os = "linux")]
fn assert_python_inspects_rust_snapshot(directory: &std::path::Path, manifest: &serde_json::Value) {
    // Exercise the real exporter and helper together. A separate legacy-shape
    // fixture references the same immutable Parquet bytes; the v2 manifest stays
    // untouched, as it would in an investigation directory.
    let mut legacy = manifest.clone();
    legacy["schema_version"] = serde_json::json!(1);
    let schemas = legacy.as_object_mut().unwrap().remove("schemas").unwrap();
    for dataset in ["source_parts", "filtered_parts"] {
        for part in legacy[dataset].as_array_mut().unwrap() {
            let id = part.as_object_mut().unwrap().remove("schema_id").unwrap();
            part["fields"] = schemas
                .as_array()
                .unwrap()
                .iter()
                .find(|schema| schema["schema_id"] == id)
                .unwrap()["fields"]
                .clone();
        }
    }
    let legacy_path = directory.join("legacy-inspection-fixture.json");
    fs::write(&legacy_path, serde_json::to_vec(&legacy).unwrap()).unwrap();
    let project = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../python");
    for path in [directory.join("manifest.json"), legacy_path] {
        let output = std::process::Command::new("uv")
            .args(["run", "--project"])
            .arg(&project)
            .args(["--locked", "python", "-m", "lvu_expr_helper.inspection"])
            .arg(path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "inspection failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.len() <= 32 * 1024 + 1);
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["coverage"]["read_rows"], 2);
        assert_eq!(result["coverage"]["admitted_rows"], 2);
        assert_eq!(result["sources"][0]["dataset"], "applied_view");
        let rows = result["rows"].as_array().unwrap();
        assert!(rows.iter().all(|row| row["values"]["projected"].is_null()));
        assert_eq!(
            rows.iter()
                .map(|row| row["values"]["_lvu_sequence"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn snapshot_packs_many_evaluation_batches_without_losing_rows_nulls_or_order() {
    use polars::prelude::{ParquetReader, SerReader};

    const ROWS: usize = 512;
    let root = TempDir::new().unwrap();
    let input = root.path().join("packed-snapshot.log");
    let data = (0..ROWS)
        .map(|index| {
            if index == 0 {
                "{\"value\":null}\n".to_owned()
            } else if index >= ROWS / 2 {
                format!("{{\"value\":{index},\"later\":\"present\"}}\n")
            } else {
                format!("{{\"value\":{index}}}\n")
            }
        })
        .collect::<String>();
    fs::write(&input, data).unwrap();
    let manager =
        SourceManager::new(root.path().join("capture"), line_framed_runtime_config()).unwrap();
    let handle = manager
        .start(source(SourceId::new(), &input, false))
        .await
        .unwrap();
    let mut progress = handle.subscribe();
    tokio::time::timeout(Duration::from_secs(4), async {
        while progress.borrow().state != RuntimeState::Stopped {
            progress.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    // Exact, not a floor: every later assertion here — the manifest counts, the
    // schema of each part and the sequence run — is only meaningful if the
    // capture is the file and nothing else.
    let captured_rows = handle.progress().records as usize;
    assert_eq!(
        captured_rows, ROWS,
        "a settled file capture must frame one record per line"
    );
    let (live, mut view) = configs(&root);
    view.page_records = 32;
    let raw = Arc::new(LiveRowProvider::new(live).unwrap());
    let mut adapter = NativeViewAdapter::new(raw, view).unwrap();
    adapter.register_source(handle.clone()).unwrap();
    adapter
        .register_view("view", vec![handle.source_id()])
        .unwrap();
    let snapshot = adapter
        .start_snapshot(
            "view",
            root.path().join("packed"),
            SnapshotLimits {
                page_records: 32,
                page_bytes: 4096,
                maximum_parts: 8,
                ..SnapshotLimits::default()
            },
        )
        .unwrap();
    let status = wait_snapshot(&snapshot);
    assert_eq!(status.state, SnapshotState::Complete, "{status:?}");
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(status.manifest_path.unwrap()).unwrap()).unwrap();
    assert_eq!(manifest["schema_version"], 2);
    assert_eq!(manifest["source_rows"], captured_rows);
    assert_eq!(manifest["filtered_rows"], captured_rows);
    assert!(manifest["source_parts"].as_array().unwrap().len() < captured_rows / 32);
    assert!(manifest["filtered_parts"].as_array().unwrap().len() < captured_rows / 32);
    assert_eq!(manifest["schemas"].as_array().unwrap().len(), 2);
    for sampled_source in manifest["inspection_sample"]["sources"].as_array().unwrap() {
        for sampled_part in sampled_source["parts"].as_array().unwrap() {
            let path = sampled_part["path"].as_str().unwrap();
            let part = manifest["source_parts"]
                .as_array()
                .unwrap()
                .iter()
                .chain(manifest["filtered_parts"].as_array().unwrap())
                .find(|part| part["path"] == path)
                .expect("sample path resolves to one manifest part");
            let schema_id = part["schema_id"].as_u64().unwrap();
            assert!(
                manifest["schemas"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|schema| schema["schema_id"].as_u64() == Some(schema_id))
            );
        }
    }

    for dataset in ["source_parts", "filtered_parts"] {
        let mut sequences = Vec::new();
        let mut first_value_is_null = false;
        for part in manifest[dataset].as_array().unwrap() {
            assert!(part.get("fields").is_none());
            let schema_id = part["schema_id"].as_u64().unwrap();
            assert!(schema_id <= 1);
            let frame = ParquetReader::new(
                fs::File::open(snapshot.output_dir().join(part["path"].as_str().unwrap())).unwrap(),
            )
            .finish()
            .unwrap();
            if sequences.is_empty() {
                first_value_is_null = frame.column("value").unwrap().get(0).unwrap().is_null();
            }
            if schema_id == 0 {
                assert!(frame.column("later").is_err());
            } else {
                assert_eq!(
                    frame.column("later").unwrap().str().unwrap().get(0),
                    Some("present")
                );
            }
            sequences.extend(
                frame
                    .column("_lvu_sequence")
                    .unwrap()
                    .u64()
                    .unwrap()
                    .into_no_null_iter(),
            );
        }
        assert!(first_value_is_null);
        assert_eq!(sequences, (0..captured_rows as u64).collect::<Vec<_>>());
    }
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

    let part_limited = adapter
        .start_snapshot(
            "view",
            root.path().join("part-limited"),
            SnapshotLimits {
                page_records: 8,
                page_bytes: 4096,
                maximum_parts: 1,
                ..SnapshotLimits::default()
            },
        )
        .unwrap();
    let part_status = wait_snapshot(&part_limited);
    assert_eq!(part_status.state, SnapshotState::Limited);
    assert!(!part_limited.output_dir().join("manifest.json").exists());
    assert!(!part_limited.output_dir().join("source").exists());
    assert!(!part_limited.output_dir().join("filtered").exists());

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
    drop(part_limited);
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
    fs::write(&input, "keep old generation\n".repeat(20)).unwrap();
    let capture_root = root.path().join("capture");
    let source_id = SourceId::new();
    let manager = SourceManager::new(&capture_root, runtime_config()).unwrap();
    let original = manager
        .start(source(source_id, &input, false))
        .await
        .unwrap();
    wait_runtime(&original, 20).await;
    let (live, view) = configs(&root);
    let raw = Arc::new(LiveRowProvider::new(live).unwrap());
    let mut adapter = NativeViewAdapter::new(raw, view).unwrap();
    adapter.register_source(original).unwrap();
    adapter.register_view("view", vec![source_id]).unwrap();
    let mut grouped = request("view", 1, 1, 0, None, None);
    grouped.purpose = QueryPurpose::Grouping;
    grouped.constraints.grouping = Some(r"^\s+".into());
    adapter.submit(grouped).unwrap();
    assert!(wait_completion(&mut adapter, 1).await.result.is_ok());
    manager.shutdown().await;

    fs::write(&input, b"  continuation in new generation\n").unwrap();
    let restarted_manager = SourceManager::new(&capture_root, runtime_config()).unwrap();
    let restarted = restarted_manager
        .start(source(source_id, &input, false))
        .await
        .unwrap();
    assert!(restarted.progress().generation > 1);
    adapter.register_source(restarted.clone()).unwrap();
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
    wait_runtime(&restarted, 1).await;
    let refreshed = tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            adapter.drain_updates(64);
            let page = adapter
                .rows()
                .page("view", ViewportRequest { start: 0, len: 4 });
            if page.total == 1
                && page.rows.first().is_some_and(|row| {
                    row.text.contains("orphan continuation") && row.text.contains("new generation")
                })
            {
                break page.rows;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(refreshed.len(), 1, "old generation groups were discarded");
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
    applied.constraints.enrichments = enrichment(expression);
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
    query.constraints.enrichments = enrichment("copy = pl.col('raw')");
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn extracted_time_follows_enrichment_preserves_dependencies_and_exports_exact_times() {
    use polars::prelude::{ParquetReader, SerReader};
    let root = TempDir::new().unwrap();
    // Raw timestamp deliberately disagrees with the extracted field.
    let input = concat!(
        "stamp<2026-09-05T12:30:45Z> timestamp=2020-01-01T00:00:00Z first\n",
        "stamp<  2026-09-05T12:30:45.250000Z  > padded\n",
        "stamp<2026-09-05T12:30:45.750000 UTC> utc-suffix\n",
        "stamp<2026-09-05T12:30:46Z> boundary\n",
        "stamp<bad> malformed\n",
        "stamp<2026-09-05T12:30:60Z> invalid-second\n",
        "timestamp=2026-09-05T12:30:45Z missing-derived\n",
    );
    // As above: the diagnostic counts are exact, and a fragment would add one.
    let (manager, handle, mut adapter) =
        setup_bytes_with_runtime(&root, input.as_bytes(), true, line_framed_runtime_config()).await;
    let mut applied = request("view", 1, 1, 0, None, None);
    applied.constraints.enrichments = enrichment(r"/stamp<(?P<timestamp_utc>[^>]+)>/");
    applied.constraints.time_basis = lvu::TimeBasis::Extracted;
    applied.constraints.capture_time = Some(lvu::CaptureTimeRange {
        start_unix_nanos: 1_788_611_445_000_000_000,
        end_unix_nanos: 1_788_611_446_000_000_000,
    });
    adapter.submit(applied.clone()).unwrap();
    assert!(wait_completion(&mut adapter, 1).await.result.is_ok());
    let rows = wait_page(&mut adapter, 3).await;
    assert!(rows[0].text.contains("first"));
    assert!(rows[1].text.contains("padded"));
    assert!(rows[2].text.contains("utc-suffix"));
    let diagnostic = adapter.status("view").unwrap().diagnostic.unwrap();
    assert!(diagnostic.contains("1 missing"), "{diagnostic}");
    assert!(diagnostic.contains("2 invalid"), "{diagnostic}");

    let mut remove = request("view", 2, 2, 1, None, None);
    remove.base_constraints = applied.constraints.clone();
    remove.constraints = applied.constraints.clone();
    remove.constraints.enrichments.clear();
    remove.purpose = QueryPurpose::Enrichment;
    adapter.submit(remove).unwrap();
    let failure = wait_completion(&mut adapter, 2).await.result.unwrap_err();
    assert_eq!(failure.purpose, QueryPurpose::Enrichment);
    assert!(wait_page(&mut adapter, 1).await[0].text.contains("first"));

    let mut file = OpenOptions::new()
        .append(true)
        .open(root.path().join("input.log"))
        .unwrap();
    file.write_all(b"stamp<2026-09-05T12:30:45.500000Z> late\n")
        .unwrap();
    file.flush().unwrap();
    wait_runtime(&handle, 5).await;
    let rows = wait_page(&mut adapter, 4).await;
    assert!(rows[3].text.contains("late"));
    assert_eq!(adapter.compiler_calls(), 0);

    let snapshot = adapter
        .start_snapshot(
            "view",
            root.path().join("snapshot"),
            SnapshotLimits::default(),
        )
        .unwrap();
    let status = wait_snapshot(&snapshot);
    assert_eq!(status.state, SnapshotState::Complete, "{:?}", status);
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(status.manifest_path.unwrap()).unwrap()).unwrap();
    assert_eq!(manifest["view"]["time_basis"], "extracted_timestamp_utc");
    assert_eq!(manifest["filtered_rows"], 4);
    let mut exported = Vec::new();
    for part in manifest["filtered_parts"].as_array().unwrap() {
        let frame = ParquetReader::new(
            fs::File::open(snapshot.output_dir().join(part["path"].as_str().unwrap())).unwrap(),
        )
        .finish()
        .unwrap();
        let times = frame
            .column("_lvu_selected_time_unix_nanos")
            .unwrap()
            .i64()
            .unwrap();
        exported.extend((0..times.len()).map(|index| times.get(index)));
    }
    assert_eq!(
        exported,
        vec![
            Some(1_788_611_445_000_000_000),
            Some(1_788_611_445_250_000_000),
            Some(1_788_611_445_750_000_000),
            Some(1_788_611_445_500_000_000)
        ]
    );

    let mut clear = request("view", 3, 3, 1, None, None);
    clear.base_constraints = applied.constraints.clone();
    clear.constraints = applied.constraints;
    clear.constraints.capture_time = None;
    adapter.submit(clear).unwrap();
    assert!(wait_completion(&mut adapter, 3).await.result.is_ok());
    assert_eq!(wait_page(&mut adapter, 5).await.len(), 5);
    adapter.shutdown();
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn raw_context_exposes_hidden_neighbors_without_changing_membership_or_crossing_sources() {
    let root = TempDir::new().unwrap();
    let (manager, handle, mut adapter) = setup(&root, "before\nneedle\nafter\n", false).await;
    let other = root.path().join("other.log");
    fs::write(&other, "other-source-secret\n").unwrap();
    let other = manager
        .start(source(SourceId::new(), &other, false))
        .await
        .unwrap();
    wait_runtime(&other, 1).await;
    adapter.register_source(other.clone()).unwrap();
    adapter
        .register_view("merged", vec![handle.source_id(), other.source_id()])
        .unwrap();
    adapter
        .submit(request("view", 1, 1, 0, Some("needle"), None))
        .unwrap();
    assert!(wait_completion(&mut adapter, 1).await.result.is_ok());
    let anchor = wait_page(&mut adapter, 1).await[0].id.clone();
    let context = tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            adapter.drain_updates(64);
            let page = adapter.rows().context_page("view", &anchor, -1, 32);
            if !page.pending && page.rows.len() == 3 {
                break page;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        context
            .rows
            .iter()
            .map(|row| row.text.as_str())
            .collect::<Vec<_>>(),
        vec!["before", "needle", "after"]
    );
    assert_eq!(adapter.status("view").unwrap().matched_records, 1);
    assert_eq!(wait_page(&mut adapter, 1).await[0].id, anchor);
    let merged = adapter
        .rows()
        .context_page("merged", &anchor, -100, usize::MAX);
    assert!(
        merged
            .rows
            .iter()
            .all(|row| row.id.source_id == anchor.source_id)
    );
    assert_eq!(merged.total, 3);
    assert!(merged.rows.len() <= 32);
    adapter.shutdown();
    for (_, report) in manager.shutdown().await {
        assert!(report.unwrap().complete);
    }
}

/// The product feature: two sources that name the same identity differently,
/// one value, every matching record from both — and nothing else.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_correlated_value_gathers_records_from_sources_that_name_the_field_differently() {
    let root = TempDir::new().unwrap();
    let api = concat!(
        r#"{"request_id":"req-7","service":"api","msg":"accepted"}"#,
        "\n",
        r#"{"request_id":"req-8","service":"api","msg":"other request"}"#,
        "\n",
        r#"{"request_id":"req-7","service":"api","msg":"responded"}"#,
        "\n",
    );
    let (manager, api_handle, mut adapter) = setup(&root, api, false).await;
    let worker_path = root.path().join("worker.log");
    fs::write(
        &worker_path,
        concat!(
            r#"{"req":"req-7","stage":"queued"}"#,
            "\n",
            r#"{"req":"req-9","stage":"queued"}"#,
            "\n",
            r#"{"request_id":"req-7","stage":"decoy"}"#,
            "\n",
        ),
    )
    .unwrap();
    let worker = manager
        .start(source(SourceId::new(), &worker_path, false))
        .await
        .unwrap();
    wait_runtime(&worker, 3).await;
    adapter.register_source(worker.clone()).unwrap();
    adapter
        .register_view("view", vec![api_handle.source_id(), worker.source_id()])
        .unwrap();

    // The lookup resolves the frozen record's typed value and offers each
    // source's own field names. It never proposes a mapping across names.
    adapter
        .submit_correlation_lookup(lvu_view::CorrelationLookupRequest {
            generation: 1,
            origin_view_id: "view".into(),
            origin: lvu_core::RecordId {
                source_id: api_handle.source_id(),
                sequence: 0,
            },
            field: "request_id".into(),
            sources: vec![api_handle.source_id(), worker.source_id()],
        })
        .unwrap();
    let candidate = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            adapter.drain_updates(64);
            if let Some(lookup) = adapter.take_correlation_lookups().pop() {
                break lookup;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap()
    .result
    .unwrap();
    assert_eq!(
        candidate.value,
        lvu_core::ExactScalar::string("req-7").unwrap()
    );
    let worker_fields = candidate
        .sources
        .iter()
        .find(|source| source.source_id == worker.source_id())
        .unwrap();
    assert!(worker_fields.fields.contains(&"req".to_owned()));
    assert!(!worker_fields.incomplete);

    let correlation = lvu_core::FieldCorrelation::new(
        "request_id",
        candidate.value.clone(),
        [
            (
                api_handle.source_id().0.to_string(),
                "request_id".to_owned(),
            ),
            (worker.source_id().0.to_string(), "req".to_owned()),
        ]
        .into_iter()
        .collect(),
    )
    .unwrap();
    let mut correlated = request("view", 1, 1, 0, None, None);
    correlated.constraints.exact_field = Some(correlation.clone());
    adapter.submit(correlated).unwrap();
    assert!(wait_completion(&mut adapter, 1).await.result.is_ok());
    // Source order, then physical sequence: the api source's two matches, then
    // the worker's `req` match. The worker's `request_id` decoy is excluded
    // because that source was mapped to `req`, not guessed.
    let rows = wait_page(&mut adapter, 3).await;
    assert_eq!(
        rows.iter().map(|row| row.text.as_str()).collect::<Vec<_>>(),
        vec![
            r#"{"request_id":"req-7","service":"api","msg":"accepted"}"#,
            r#"{"request_id":"req-7","service":"api","msg":"responded"}"#,
            r#"{"req":"req-7","stage":"queued"}"#,
        ]
    );
    assert_eq!(adapter.status("view").unwrap().matched_records, 3);

    // An unmapped source contributes nothing rather than falling back to the
    // origin's field name.
    let only_api = lvu_core::FieldCorrelation::new(
        "request_id",
        candidate.value,
        [(
            api_handle.source_id().0.to_string(),
            "request_id".to_owned(),
        )]
        .into_iter()
        .collect(),
    )
    .unwrap();
    let mut narrowed = request("view", 2, 2, 1, None, None);
    narrowed.base_constraints.exact_field = Some(correlation);
    narrowed.constraints.exact_field = Some(only_api);
    adapter.submit(narrowed).unwrap();
    assert!(wait_completion(&mut adapter, 2).await.result.is_ok());
    let rows = wait_page(&mut adapter, 2).await;
    assert!(
        rows.iter()
            .all(|row| row.id.source_id == api_handle.source_id().0.to_string())
    );
    assert_eq!(adapter.status("view").unwrap().matched_records, 2);

    adapter.shutdown();
    manager.shutdown().await;
}

/// A record the bounded scan cannot reach reports that, and a cancelled lookup
/// never publishes anything for the field the user moved away from.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_correlation_lookup_is_bounded_cancellable_and_explicit_about_what_it_missed() {
    let root = TempDir::new().unwrap();
    let (manager, handle, mut adapter) =
        setup(&root, "{\"id\":\"a\"}\n{\"id\":\"b\"}\n", false).await;
    adapter
        .register_view("view", vec![handle.source_id()])
        .unwrap();

    // A sequence past the journal's end is refused, not silently answered from
    // a neighbouring record.
    adapter
        .submit_correlation_lookup(lvu_view::CorrelationLookupRequest {
            generation: 1,
            origin_view_id: "view".into(),
            origin: lvu_core::RecordId {
                source_id: handle.source_id(),
                sequence: 4_000,
            },
            field: "id".into(),
            sources: vec![handle.source_id()],
        })
        .unwrap();
    let lookup = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            adapter.drain_updates(64);
            if let Some(lookup) = adapter.take_correlation_lookups().pop() {
                break lookup;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(lookup.result.is_err(), "{:?}", lookup.result);

    adapter
        .submit_correlation_lookup(lvu_view::CorrelationLookupRequest {
            generation: 2,
            origin_view_id: "view".into(),
            origin: lvu_core::RecordId {
                source_id: handle.source_id(),
                sequence: 0,
            },
            field: "id".into(),
            sources: vec![handle.source_id()],
        })
        .unwrap();
    adapter.cancel_correlation_lookup();
    for _ in 0..40 {
        adapter.drain_updates(64);
        assert!(
            adapter.take_correlation_lookups().is_empty(),
            "a cancelled lookup must not publish a result"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    adapter.shutdown();
    manager.shutdown().await;
}

/// Reproducible local measurement, intentionally excluded from ordinary tests.
/// No wall-time performance threshold: report this host's measurements and
/// assert correctness/bounds under concurrent capture instead.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "local capture/query benchmark; run with --ignored --nocapture"]
async fn measure_capture_and_historical_queries_under_small_cache_budgets() {
    const INITIAL: usize = 50_000;
    const ADDED: usize = 10_000;
    fn records(start: usize, count: usize) -> String {
        let mut output = String::with_capacity(count * 70);
        for index in start..start + count {
            use std::fmt::Write;
            writeln!(
                output,
                "{{\"id\":{index},\"status\":{},\"message\":\"{}\"}}",
                if index % 10 == 0 { 500 } else { 200 },
                if index % 10 == 0 {
                    "failure"
                } else {
                    "healthy"
                }
            )
            .unwrap();
        }
        output
    }
    let root = TempDir::new().unwrap();
    let path = root.path().join("benchmark.log");
    let initial = records(0, INITIAL);
    fs::write(&path, initial.as_bytes()).unwrap();
    let input_bytes = initial.len();
    drop(initial);
    let mut runtime = RuntimeConfig::default();
    runtime.acquisition.read_chunk_bytes = 64 * 1024;
    runtime.acquisition.partial_flush_interval = Duration::from_secs(60);
    runtime.batch_records = 256;
    runtime.max_page_records = 512;
    runtime.max_page_bytes = 1024 * 1024;
    let manager = SourceManager::new(root.path().join("capture"), runtime).unwrap();
    let start = std::time::Instant::now();
    let handle = manager
        .start(source(SourceId::new(), &path, true))
        .await
        .unwrap();
    let (mut live, mut query) = configs(&root);
    live.cache_rows = 32;
    live.cache_bytes = 128 * 1024;
    live.index_page_records = 256;
    live.index_page_bytes = 1024 * 1024;
    live.maximum_index_bytes_per_source = 8 * 1024 * 1024;
    live.maximum_total_index_bytes = 8 * 1024 * 1024;
    query.page_records = 512;
    query.page_bytes = 1024 * 1024;
    query.maximum_index_bytes = 128 * 1024;
    let raw = Arc::new(LiveRowProvider::new(live).unwrap());
    let mut adapter = NativeViewAdapter::new(raw, query).unwrap();
    adapter.register_source(handle.clone()).unwrap();
    adapter
        .register_view("view", vec![handle.source_id()])
        .unwrap();
    let mut progress = handle.subscribe();
    tokio::time::timeout(Duration::from_secs(60), async {
        while progress.borrow().records < INITIAL as u64 {
            progress.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    println!(
        "MEASURE initial_records={INITIAL} input_bytes={input_bytes} capture_ms={}",
        start.elapsed().as_millis()
    );
    let start = std::time::Instant::now();
    adapter
        .submit(request("view", 1, 1, 0, Some("failure"), None))
        .unwrap();
    assert!(wait_completion(&mut adapter, 1).await.result.is_ok());
    assert_eq!(
        adapter.status("view").unwrap().matched_records,
        (INITIAL / 10) as u64
    );
    println!(
        "MEASURE literal_scan_ms={} matches={}",
        start.elapsed().as_millis(),
        INITIAL / 10
    );
    let producer = std::thread::spawn(move || {
        let mut file = OpenOptions::new().append(true).open(path).unwrap();
        for burst in 0..10 {
            file.write_all(records(INITIAL + burst * 1000, 1000).as_bytes())
                .unwrap();
            file.flush().unwrap();
            std::thread::sleep(Duration::from_millis(25));
        }
    });
    let start = std::time::Instant::now();
    let advanced = "pl.col('status') >= 500";
    let definition = with_base(
        request("view", 2, 2, 1, Some("failure"), Some(advanced)),
        Some("failure"),
        None,
    );
    let constraints = definition.constraints.clone();
    adapter.submit(definition).unwrap();
    assert!(wait_completion(&mut adapter, 2).await.result.is_ok());
    println!(
        "MEASURE advanced_compile_and_scan_ms={}",
        start.elapsed().as_millis()
    );
    producer.join().unwrap();
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            adapter.drain_updates(64);
            let status = adapter.status("view").unwrap();
            assert!(
                !matches!(status.state, ScanState::Error | ScanState::Limited),
                "{status:?}"
            );
            if status.matched_records == ((INITIAL + ADDED) / 10) as u64 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    println!(
        "MEASURE append_and_catchup_ms={} total_records={}",
        start.elapsed().as_millis(),
        handle.progress().records
    );
    let start = std::time::Instant::now();
    let mut base = constraints;
    for revision in 3..=5 {
        let mut refresh = request(
            "view",
            revision,
            revision,
            revision - 1,
            Some("failure"),
            Some(advanced),
        );
        refresh.base_constraints = base.clone();
        refresh.constraints.capture_time = Some(lvu::CaptureTimeRange {
            start_unix_nanos: revision as i64,
            end_unix_nanos: i64::MAX,
        });
        base = refresh.constraints.clone();
        adapter.submit(refresh).unwrap();
        assert!(wait_completion(&mut adapter, revision).await.result.is_ok());
    }
    println!(
        "MEASURE three_time_revisions_ms={}",
        start.elapsed().as_millis()
    );
    let start = std::time::Instant::now();
    OpenOptions::new()
        .append(true)
        .open(root.path().join("benchmark.log"))
        .unwrap()
        .write_all(records(INITIAL + ADDED, 1000).as_bytes())
        .unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            adapter.drain_updates(64);
            if adapter.status("view").unwrap().matched_records
                == ((INITIAL + ADDED + 1000) / 10) as u64
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    println!(
        "MEASURE warm_incremental_1000_ms={}",
        start.elapsed().as_millis()
    );
    let start = std::time::Instant::now();
    for position in [0, 1000, 2000, 3000, 6000, 0] {
        let page = tokio::time::timeout(Duration::from_secs(4), async {
            loop {
                adapter.drain_updates(64);
                let page = adapter.rows().page(
                    "view",
                    ViewportRequest {
                        start: position,
                        len: 8,
                    },
                );
                if page.rows.len() == 8 {
                    break page;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(page.rows[0].id.sequence, position as u64 * 10);
        let cache = adapter.raw_stats();
        assert!(cache.cached_rows <= 32 && cache.cached_bytes <= 128 * 1024);
    }
    println!(
        "MEASURE six_cold_viewports_ms={}",
        start.elapsed().as_millis()
    );
    let page = wait_page(&mut adapter, 8).await;
    assert_eq!(page[0].id.sequence, 0);
    assert_eq!(page[7].id.sequence, 70);
    let status = adapter.status("view").unwrap();
    let cache = adapter.raw_stats();
    assert_eq!(
        status.matched_records,
        ((INITIAL + ADDED + 1000) / 10) as u64
    );
    assert!(status.index_bytes <= 128 * 1024);
    assert!(cache.cached_bytes <= 128 * 1024 && cache.cached_rows <= 32);
    println!(
        "MEASURE membership_bytes={} cache_bytes={} cache_rows={} pending_requests={}",
        status.index_bytes, cache.cached_bytes, cache.cached_rows, cache.pending_requests
    );
    adapter.shutdown();
    for (_, result) in manager.shutdown().await {
        assert!(result.unwrap().complete);
    }
}

/// Two minutes of paced capture with concurrent native queries and cold paging.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "two-minute sustained acceptance; run bench:live:sustained"]
async fn measure_sustained_capture_queries_and_bounded_paging() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::AsyncWriteExt;
    const INITIAL: usize = 1_000;
    const BURSTS: usize = 1_200;
    const PER_BURST: usize = 400;
    const EXPECTED: usize = INITIAL + BURSTS * PER_BURST;
    fn records(start: usize, count: usize) -> String {
        let mut text = String::with_capacity(count * 64);
        for id in start..start + count {
            use std::fmt::Write;
            writeln!(
                text,
                "{{\"id\":{id},\"status\":{},\"message\":\"{}\"}}",
                if id % 100 == 0 { 500 } else { 200 },
                if id % 100 == 0 { "failure" } else { "healthy" }
            )
            .unwrap();
        }
        text
    }
    let root = TempDir::new().unwrap();
    let path = root.path().join("sustained.log");
    fs::write(&path, records(0, INITIAL)).unwrap();
    let mut runtime = RuntimeConfig::default();
    runtime.acquisition.read_chunk_bytes = 64 * 1024;
    runtime.acquisition.partial_flush_interval = Duration::from_secs(60);
    runtime.batch_records = 256;
    runtime.max_page_records = 512;
    runtime.max_page_bytes = 1024 * 1024;
    let manager = SourceManager::new(root.path().join("capture"), runtime).unwrap();
    let handle = manager
        .start(source(SourceId::new(), &path, true))
        .await
        .unwrap();
    let (mut live, mut query) = configs(&root);
    live.cache_rows = 32;
    live.cache_bytes = 128 * 1024;
    live.index_page_records = 256;
    live.index_page_bytes = 1024 * 1024;
    live.maximum_index_bytes_per_source = 32 * 1024 * 1024;
    live.maximum_total_index_bytes = 32 * 1024 * 1024;
    query.page_records = 512;
    query.page_bytes = 1024 * 1024;
    query.maximum_index_bytes = 128 * 1024;
    let mut adapter =
        NativeViewAdapter::new(Arc::new(LiveRowProvider::new(live).unwrap()), query).unwrap();
    adapter.register_source(handle.clone()).unwrap();
    adapter
        .register_view("view", vec![handle.source_id()])
        .unwrap();
    let mut progress = handle.subscribe();
    tokio::time::timeout(Duration::from_secs(30), async {
        while progress.borrow().records < INITIAL as u64 {
            progress.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    let advanced = "pl.col('status') >= 500";
    let initial = request("view", 1, 1, 0, Some("failure"), Some(advanced));
    let mut base = initial.constraints.clone();
    adapter.submit(initial).unwrap();
    assert!(wait_completion(&mut adapter, 1).await.result.is_ok());
    let produced = Arc::new(AtomicUsize::new(INITIAL));
    let producer_count = produced.clone();
    let start = std::time::Instant::now();
    let producer = tokio::spawn(async move {
        let mut file = tokio::fs::OpenOptions::new()
            .append(true)
            .open(path)
            .await
            .unwrap();
        let mut interval = tokio::time::interval(Duration::from_millis(100));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        for burst in 0..BURSTS {
            interval.tick().await;
            file.write_all(records(INITIAL + burst * PER_BURST, PER_BURST).as_bytes())
                .await
                .unwrap();
            producer_count.store(INITIAL + (burst + 1) * PER_BURST, Ordering::Release);
        }
        file.sync_data().await.unwrap();
    });
    let mut revision = 1;
    let mut pending = false;
    let mut last_report = 0;
    let mut max_capture_lag = 0;
    let mut max_query_lag = 0;
    let mut checked_pages = 0usize;
    tokio::time::timeout(Duration::from_secs(190), async {
        loop {
            adapter.drain_updates(64);
            while let Some(completion) = adapter.poll() {
                assert!(completion.result.is_ok(), "{completion:?}");
                pending = false;
            }
            let elapsed = start.elapsed().as_secs();
            if !pending && revision < 4 && elapsed >= revision * 30 {
                let mut next = request("view", revision + 1, revision + 1, revision, Some("failure"), Some(advanced));
                next.base_constraints = base.clone();
                next.constraints.capture_time = Some(lvu::CaptureTimeRange { start_unix_nanos: 0, end_unix_nanos: i64::MAX - revision as i64 });
                base = next.constraints.clone();
                adapter.submit(next).unwrap();
                revision += 1;
                pending = true;
            }
            let status = adapter.status("view").unwrap();
            assert!(!matches!(status.state, ScanState::Error | ScanState::Limited), "{status:?}");
            let written = produced.load(Ordering::Acquire) as u64;
            max_capture_lag = max_capture_lag.max(written.saturating_sub(handle.progress().records));
            max_query_lag = max_query_lag.max(written.div_ceil(100).saturating_sub(status.matched_records));
            let position = if (elapsed / 5).is_multiple_of(2) { 0 } else { status.matched_records.saturating_sub(8) as usize };
            let page = adapter.rows().page("view", ViewportRequest { start: position, len: 8 });
            for (offset, row) in page.rows.iter().enumerate() { assert_eq!(row.id.sequence, ((position + offset) * 100) as u64); }
            if !page.rows.is_empty() { checked_pages += 1; }
            let cache = adapter.raw_stats();
            assert!(cache.cached_rows <= 32 && cache.cached_bytes <= 128 * 1024);
            assert!(status.index_bytes <= 128 * 1024);
            if elapsed / 10 > last_report {
                last_report = elapsed / 10;
                println!("SUSTAINED seconds={elapsed} produced={written} captured={} matched={} cache_rows={} membership_bytes={}", handle.progress().records, status.matched_records, cache.cached_rows, status.index_bytes);
            }
            if producer.is_finished() && !pending && status.matched_records == EXPECTED.div_ceil(100) as u64 { break; }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await.unwrap();
    producer.await.unwrap();
    assert_eq!(handle.progress().records, EXPECTED as u64);
    assert!(checked_pages > 0);
    let peak_rss = fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|text| {
            text.lines().find_map(|line| {
                line.strip_prefix("VmHWM:")
                    .map(str::trim)
                    .map(str::to_owned)
            })
        });
    println!(
        "SUSTAINED complete_ms={} records={EXPECTED} max_capture_lag_records={max_capture_lag} max_query_lag_matches={max_query_lag} checked_pages={checked_pages} process_peak_rss={peak_rss:?}",
        start.elapsed().as_millis()
    );
    adapter.shutdown();
    for (_, report) in manager.shutdown().await {
        assert!(report.unwrap().complete);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_membership_changes_publish_atomically_and_preserve_failed_or_superseded_views() {
    let root = TempDir::new().unwrap();
    let (manager, first, mut adapter) = setup(&root, "keep first\ndrop first\n", true).await;
    let path = root.path().join("second.log");
    fs::write(&path, "keep second\ndrop second\n").unwrap();
    let second = manager
        .start(source(SourceId::new(), &path, true))
        .await
        .unwrap();
    wait_runtime(&second, 2).await;
    adapter.register_source(second.clone()).unwrap();
    let first_id = first.source_id();
    let second_id = second.source_id();
    let initial = request("view", 1, 1, 0, Some("keep"), None);
    adapter.submit(initial.clone()).unwrap();
    assert!(wait_completion(&mut adapter, 1).await.result.is_ok());
    let original = wait_page(&mut adapter, 1).await[0].id.clone();

    let mut merge = request("view", 2, 2, 1, Some("keep"), None);
    merge.base_constraints = initial.constraints.clone();
    adapter
        .submit_source_change(merge.clone(), vec![second_id, first_id])
        .unwrap();
    assert_eq!(adapter.view_sources("view"), Some(vec![first_id]));
    assert_eq!(
        adapter
            .rows()
            .page("view", ViewportRequest { start: 0, len: 4 })
            .rows[0]
            .id,
        original
    );
    assert!(second.stop().await.unwrap().complete);
    let restarted = manager.start(source(second_id, &path, true)).await.unwrap();
    wait_runtime(&restarted, 2).await;
    adapter.register_source(restarted).unwrap();
    assert!(wait_completion(&mut adapter, 2).await.result.is_ok());
    let merged = wait_page(&mut adapter, 2).await;
    // Both sources are in the view, and the reordered list published whole.
    // The *row* order no longer reports the list order: a merged view
    // interleaves by the basis time, and the source list only breaks ties
    // (docs/merged-view-ordering.md). These two records were captured at
    // different instants, so capture time decides, and the list order is
    // asserted where it is actually visible — `view_sources`, below.
    assert_eq!(
        merged
            .iter()
            .map(|row| row.id.source_id.clone())
            .collect::<std::collections::BTreeSet<_>>(),
        [second_id.0.to_string(), first_id.0.to_string()]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
    );
    // The record that was there before the merge is still that record, at
    // whichever index the merge gave it.
    let index = adapter
        .rows()
        .index_of_id("view", &original)
        .expect("the original record still resolves");
    assert_eq!(merged[index].id, original);
    assert_eq!(
        adapter.view_sources("view"),
        Some(vec![second_id, first_id])
    );

    let snapshot = adapter
        .start_snapshot(
            "view",
            root.path().join("merged-export"),
            SnapshotLimits::default(),
        )
        .unwrap();
    assert_eq!(wait_snapshot(&snapshot).state, SnapshotState::Complete);
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(snapshot.output_dir().join("manifest.json")).unwrap())
            .unwrap();
    assert_eq!(manifest["filtered_rows"], 2);
    assert_eq!(manifest["sources"].as_array().unwrap().len(), 2);

    let mut invalid = request("view", 3, 3, 2, Some("/[/"), None);
    invalid.base_constraints = merge.constraints.clone();
    adapter
        .submit_source_change(invalid, vec![first_id])
        .unwrap();
    assert!(wait_completion(&mut adapter, 3).await.result.is_err());
    assert_eq!(
        adapter.view_sources("view"),
        Some(vec![second_id, first_id])
    );
    assert_eq!(wait_page(&mut adapter, 2).await, merged);

    let mut candidate = request("view", 4, 4, 2, Some("keep"), None);
    candidate.base_constraints = merge.constraints.clone();
    adapter
        .submit_source_change(candidate, vec![first_id])
        .unwrap();
    let mut superseding = request("view", 5, 5, 2, Some("drop"), None);
    superseding.base_constraints = merge.constraints;
    adapter.submit(superseding.clone()).unwrap();
    assert!(wait_completion(&mut adapter, 5).await.result.is_ok());
    assert_eq!(
        adapter.view_sources("view"),
        Some(vec![second_id, first_id])
    );
    let rows = wait_page(&mut adapter, 2).await;
    assert!(rows.iter().all(|row| row.text.starts_with("drop")));

    let mut clear = request("view", 6, 6, 5, None, None);
    clear.base_constraints = superseding.constraints;
    adapter.submit_source_change(clear, vec![first_id]).unwrap();
    assert!(wait_completion(&mut adapter, 6).await.result.is_ok());
    assert_eq!(adapter.view_sources("view"), Some(vec![first_id]));
    assert!(
        wait_page(&mut adapter, 2)
            .await
            .iter()
            .all(|row| row.id.source_id == first_id.0.to_string())
    );
    assert!(
        adapter
            .submit_source_change(request("view", 7, 7, 6, None, None), vec![])
            .is_err()
    );
    assert!(
        adapter
            .submit_source_change(
                request("view", 7, 7, 6, None, None),
                vec![first_id, first_id]
            )
            .is_err()
    );
    assert!(
        adapter
            .submit_source_change(request("view", 7, 7, 6, None, None), vec![SourceId::new()])
            .is_err()
    );
    assert_eq!(adapter.view_sources("view"), Some(vec![first_id]));
    adapter.shutdown();
    for (_, report) in manager.shutdown().await {
        assert!(report.unwrap().complete);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_membership_publication_failure_keeps_raw_registration_and_query_base() {
    let root = TempDir::new().unwrap();
    let manager = SourceManager::new(root.path().join("capture"), runtime_config()).unwrap();
    let mut handles = Vec::new();
    for name in ["first", "second"] {
        let path = root.path().join(name);
        fs::write(&path, format!("keep {name}\n")).unwrap();
        let handle = manager
            .start(source(SourceId::new(), &path, false))
            .await
            .unwrap();
        wait_runtime(&handle, 1).await;
        handles.push(handle);
    }
    let (mut live, config) = configs(&root);
    live.maximum_view_sources = 1;
    let raw = Arc::new(LiveRowProvider::new(live).unwrap());
    let mut adapter = NativeViewAdapter::new(raw, config).unwrap();
    for handle in &handles {
        adapter.register_source(handle.clone()).unwrap();
    }
    let first = handles[0].source_id();
    adapter.register_view("view", vec![first]).unwrap();
    let original = wait_page(&mut adapter, 1).await[0].id.clone();
    adapter
        .submit_source_change(
            request("view", 1, 1, 0, Some("keep"), None),
            handles.iter().map(SourceHandle::source_id).collect(),
        )
        .unwrap();
    assert!(
        wait_completion(&mut adapter, 1)
            .await
            .result
            .unwrap_err()
            .message
            .contains("source membership publication")
    );
    assert_eq!(adapter.view_sources("view"), Some(vec![first]));
    assert_eq!(wait_page(&mut adapter, 1).await[0].id, original);
    adapter
        .submit(request("view", 2, 2, 0, Some("keep"), None))
        .unwrap();
    assert!(wait_completion(&mut adapter, 2).await.result.is_ok());
    assert_eq!(wait_page(&mut adapter, 1).await[0].id, original);
    adapter.shutdown();
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn frozen_input_visits_accepted_native_fields_membership_and_exact_bytes() {
    let root = TempDir::new().unwrap();
    let original = concat!(
        "{\"message\":\"drop β\",\"value\":1}\n",
        "{\"message\":\"keep é\",\"value\":2}\n",
        "{\"message\":null,\"value\":3}\n"
    );
    let (manager, handle, mut adapter) = setup(&root, original, true).await;
    let mut applied = request("view", 4, 4, 0, None, Some("pl.col('value') >= 2"));
    applied.purpose = QueryPurpose::Enrichment;
    applied.constraints.enrichments = vec![
        lvu::EnrichmentDefinition {
            id: lvu::EnrichmentStageId("upper".into()),
            source: "upper = pl.col('message').str.to_uppercase()".into(),
            command: None,
        },
        lvu::EnrichmentDefinition {
            id: lvu::EnrichmentStageId("dependent".into()),
            source: "dependent = pl.col('upper').fill_null('MISSING').str.slice(0, 4)".into(),
            command: None,
        },
    ];
    adapter.submit(applied).unwrap();
    assert!(wait_completion(&mut adapter, 4).await.result.is_ok());

    let frozen = adapter
        .freeze_input(
            "view",
            FrozenInputLimits {
                batch_records: 1,
                batch_bytes: 4096,
                ..FrozenInputLimits::default()
            },
        )
        .unwrap();
    assert_eq!(frozen.summary().applied_revision, 4);
    assert_eq!(frozen.summary().selected_records, Some(2));
    assert_eq!(frozen.summary().sources[0].high_watermark, Some(2));

    let mut file = OpenOptions::new()
        .append(true)
        .open(root.path().join("input.log"))
        .unwrap();
    writeln!(file, "{{\"message\":\"keep later\",\"value\":4}}").unwrap();
    file.flush().unwrap();
    wait_runtime(&handle, 4).await;

    let visited = std::thread::spawn(move || {
        let mut rows = Vec::new();
        let stats = frozen
            .visit(&AtomicBool::new(false), |batch| {
                assert_eq!(batch.rows.len(), 1);
                rows.extend(batch.rows);
                Ok(())
            })
            .unwrap();
        (stats, rows)
    })
    .join()
    .unwrap();
    assert_eq!(visited.0.scanned_records, 3);
    assert_eq!(visited.0.output_records, 2);
    assert_eq!(
        visited
            .1
            .iter()
            .map(|row| row.record.record_id.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(
        visited.1[0].record.bytes,
        r#"{"message":"keep é","value":2}"#.as_bytes().to_vec()
    );
    assert_eq!(visited.1[0].record.delimiter, b"\n");
    assert_eq!(visited.1[0].fields["message"], "keep é");
    assert_eq!(visited.1[0].fields["value"], 2);
    assert_eq!(visited.1[0].fields["upper"], "KEEP É");
    assert_eq!(visited.1[0].fields["dependent"], "KEEP");
    assert_eq!(visited.1[1].fields["message"], serde_json::Value::Null);
    assert_eq!(visited.1[1].fields["dependent"], "MISS");
    assert!(
        visited
            .1
            .iter()
            .all(|row| !row.fields.contains_key("_lvu_raw"))
    );

    adapter.shutdown();
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn frozen_input_honors_cancellation_limits_and_visitor_failures() {
    let root = TempDir::new().unwrap();
    let (manager, _handle, mut adapter) = setup(&root, "one\ntwo\n", false).await;
    let cancelled = adapter
        .freeze_input("view", FrozenInputLimits::default())
        .unwrap();
    let cancelled_result =
        std::thread::spawn(move || cancelled.visit(&AtomicBool::new(true), |_| Ok(())))
            .join()
            .unwrap();
    assert!(matches!(cancelled_result, Err(FrozenInputError::Cancelled)));

    let between_batches = adapter
        .freeze_input(
            "view",
            FrozenInputLimits {
                batch_records: 1,
                batch_bytes: 4096,
                ..FrozenInputLimits::default()
            },
        )
        .unwrap();
    let between_result = std::thread::spawn(move || {
        let flag = AtomicBool::new(false);
        let mut batches = 0;
        let result = between_batches.visit(&flag, |_| {
            batches += 1;
            flag.store(true, std::sync::atomic::Ordering::Release);
            Ok(())
        });
        (result, batches)
    })
    .join()
    .unwrap();
    assert!(matches!(between_result.0, Err(FrozenInputError::Cancelled)));
    assert_eq!(between_result.1, 1);

    let limited = adapter
        .freeze_input(
            "view",
            FrozenInputLimits {
                maximum_output_records: 1,
                ..FrozenInputLimits::default()
            },
        )
        .unwrap();
    let limited_result =
        std::thread::spawn(move || limited.visit(&AtomicBool::new(false), |_| Ok(())))
            .join()
            .unwrap();
    assert!(matches!(
        limited_result,
        Err(FrozenInputError::Limited(message)) if message.contains("output record")
    ));

    let failed = adapter
        .freeze_input("view", FrozenInputLimits::default())
        .unwrap();
    let failed_result = std::thread::spawn(move || {
        failed.visit(&AtomicBool::new(false), |_| Err("caller stopped".into()))
    })
    .join()
    .unwrap();
    assert!(matches!(
        failed_result,
        Err(FrozenInputError::Visitor(message)) if message == "caller stopped"
    ));

    let first_lease = adapter
        .freeze_input("view", FrozenInputLimits::default())
        .unwrap();
    let second_lease = adapter
        .freeze_input("view", FrozenInputLimits::default())
        .unwrap();
    assert!(
        adapter
            .freeze_input("view", FrozenInputLimits::default())
            .is_err()
    );
    drop((first_lease, second_lease));
    adapter.shutdown();
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn frozen_input_rejects_lossy_structured_projection() {
    let root = TempDir::new().unwrap();
    let (manager, _handle, mut adapter) =
        setup(&root, "{\"nested\":{\"answer\":42}}\n", false).await;
    let frozen = adapter
        .freeze_input("view", FrozenInputLimits::default())
        .unwrap();
    assert_eq!(frozen.summary().selected_records, None);
    let result = std::thread::spawn(move || frozen.visit(&AtomicBool::new(false), |_| Ok(())))
        .join()
        .unwrap();
    assert!(matches!(
        result,
        Err(FrozenInputError::Replay(message)) if message.contains("structured input represented internally as text")
    ));
    adapter.shutdown();
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn frozen_input_rejects_failed_native_replay_before_exposing_affected_batch() {
    let root = TempDir::new().unwrap();
    let (manager, handle, mut adapter) = setup(&root, "status=200 initial\n", true).await;
    let expression = r#"status_code = pl.col("raw").str.extract(r"status=(\w+)", 1).cast(pl.Int64, strict=True)"#;
    let mut applied = request("view", 1, 1, 0, None, None);
    applied.purpose = QueryPurpose::Enrichment;
    applied.constraints.enrichments = enrichment(expression);
    adapter.submit(applied).unwrap();
    assert!(wait_completion(&mut adapter, 1).await.result.is_ok());

    let mut file = OpenOptions::new()
        .append(true)
        .open(root.path().join("input.log"))
        .unwrap();
    writeln!(file, "status=bad still raw").unwrap();
    file.flush().unwrap();
    wait_runtime(&handle, 2).await;
    let _ = wait_page(&mut adapter, 2).await;

    let frozen = adapter
        .freeze_input("view", FrozenInputLimits::default())
        .unwrap();
    let result = std::thread::spawn(move || {
        let mut exposed = Vec::new();
        let result = frozen.visit(&AtomicBool::new(false), |batch| {
            exposed.extend(
                batch
                    .rows
                    .into_iter()
                    .map(|row| row.record.record_id.sequence),
            );
            Ok(())
        });
        (result, exposed)
    })
    .join()
    .unwrap();
    assert!(matches!(
        result.0,
        Err(FrozenInputError::Replay(message))
            if message.contains("accepted native stage replay failed")
                && message.contains("status_code")
    ));
    assert!(!result.1.contains(&1), "the failed batch was exposed");
    adapter.shutdown();
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn frozen_input_cancelled_empty_source_is_not_success() {
    let root = TempDir::new().unwrap();
    let (manager, _handle, mut adapter) = setup(&root, "", false).await;
    let frozen = adapter
        .freeze_input("view", FrozenInputLimits::default())
        .unwrap();
    let result = std::thread::spawn(move || frozen.visit(&AtomicBool::new(true), |_| Ok(())))
        .join()
        .unwrap();
    assert!(matches!(result, Err(FrozenInputError::Cancelled)));
    adapter.shutdown();
    manager.shutdown().await;
}

/// Dataset-relative ranges and gap navigation both read the basis timestamps
/// the membership now retains. What matters is that the bounds describe the
/// *dataset* rather than whatever window is already applied, and that a gap is
/// measured between two records that both have a time.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn membership_reports_dataset_bounds_and_finds_gaps_between_timed_records() {
    let root = TempDir::new().unwrap();
    let (manager, handle, mut adapter) = setup(&root, "a\nb\nc\nd\n", false).await;
    adapter
        .submit(request("view", 1, 1, 0, None, None))
        .unwrap();
    assert!(wait_completion(&mut adapter, 1).await.result.is_ok());
    let rows = wait_page(&mut adapter, 4).await;
    let captures: Vec<i64> = rows
        .iter()
        .map(|row| row.captured_at_unix_nanos.unwrap())
        .collect();

    // Bounds cover every matched record, in the capture basis.
    let bounds = adapter
        .rows()
        .time_bounds("view", lvu::TimeBasis::Capture)
        .expect("bounds");
    assert_eq!(bounds.first_unix_nanos, *captures.iter().min().unwrap());
    assert_eq!(bounds.last_unix_nanos, *captures.iter().max().unwrap());
    assert_eq!(bounds.count, 4);
    assert_eq!(bounds.missing, 0);

    // Narrowing the view to one record must not change what the dataset's
    // bounds are: "the last N minutes of data" would otherwise shrink every
    // time it was applied.
    let mut windowed = request("view", 2, 2, 1, None, None);
    windowed.constraints.capture_time = Some(lvu::CaptureTimeRange {
        start_unix_nanos: captures[1],
        end_unix_nanos: captures[1] + 1,
    });
    adapter.submit(windowed).unwrap();
    assert!(wait_completion(&mut adapter, 2).await.result.is_ok());
    let narrowed = adapter
        .rows()
        .time_bounds("view", lvu::TimeBasis::Capture)
        .expect("bounds");
    assert_eq!(
        narrowed.first_unix_nanos, bounds.first_unix_nanos,
        "bounds are measured before the window narrows the set"
    );
    assert_eq!(narrowed.last_unix_nanos, bounds.last_unix_nanos);

    adapter.shutdown();
    manager.shutdown().await;
    drop(handle);
}

/// A raw (unfiltered) view keeps no membership, so both answers come from
/// bounded page reads instead. All events is exactly where gap navigation is
/// most useful, so answering `None` there would have made the feature useless
/// where it matters.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_raw_view_answers_bounds_and_gaps_from_bounded_page_reads() {
    let root = TempDir::new().unwrap();
    let (manager, handle, mut adapter) = setup(&root, "a\nb\nc\n", false).await;
    adapter
        .submit(request("view", 1, 1, 0, None, None))
        .unwrap();
    assert!(wait_completion(&mut adapter, 1).await.result.is_ok());
    let rows = wait_page(&mut adapter, 3).await;
    let captures: Vec<i64> = rows
        .iter()
        .map(|row| row.captured_at_unix_nanos.unwrap())
        .collect();

    let bounds = adapter
        .rows()
        .time_bounds("view", lvu::TimeBasis::Capture)
        .expect("raw bounds");
    assert_eq!(bounds.first_unix_nanos, captures[0]);
    assert_eq!(bounds.last_unix_nanos, captures[2]);
    assert_eq!(bounds.count, 3);

    // These records were written in one burst, so no gap exceeds a full second.
    assert_eq!(
        adapter.rows().find_gap(
            "view",
            None,
            lvu::GapDirection::Forward,
            1_000_000_000,
            lvu::TimeBasis::Capture
        ),
        None,
        "a burst has no gap to find"
    );
    // A threshold of nothing finds the first boundary between two records.
    let hit = adapter.rows().find_gap(
        "view",
        None,
        lvu::GapDirection::Forward,
        0,
        lvu::TimeBasis::Capture,
    );
    assert_eq!(
        hit, None,
        "a non-positive threshold is refused rather than matching everything"
    );
    adapter.shutdown();
    manager.shutdown().await;
    drop(handle);
}

fn command_step(id: &str, name: &str) -> lvu::EnrichmentDefinition {
    lvu::EnrichmentDefinition::command(
        id.to_owned(),
        name.to_owned(),
        CommandDefinition {
            program: CommandProgram::Exec {
                executable: "true".into(),
                args: Vec::new(),
            },
            cwd: None,
            environment: BTreeMap::new(),
            restart: RestartPolicy::Never,
        },
    )
}

fn expression_step(id: &str, source: &str) -> lvu::EnrichmentDefinition {
    lvu::EnrichmentDefinition::expression(id.to_owned(), source.to_owned())
}

fn chain_request(
    revision: u64,
    base_revision: u64,
    base: &[lvu::EnrichmentDefinition],
    chain: &[lvu::EnrichmentDefinition],
    advanced: Option<&str>,
) -> QueryRequest {
    let mut request = request("view", revision, revision, base_revision, None, advanced);
    request.purpose = QueryPurpose::Enrichment;
    request.base_constraints = QueryConstraints {
        enrichments: base.to_vec(),
        ..QueryConstraints::default()
    };
    request.constraints.enrichments = chain.to_vec();
    request
}

/// docs/command-enrichment.md: a command step's published results are
/// `<name>.<field>` columns that later steps and filters read; before a run
/// they are null, so a chain that reads them is valid and just says nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn command_results_join_as_columns_for_later_steps_and_filters() {
    let root = TempDir::new().unwrap();
    let (manager, handle, mut adapter) = setup(&root, "alpha\nbeta\ngamma\n", true).await;
    let chain = vec![
        command_step("command-1", "geo"),
        expression_step(
            "upper",
            "city_upper = pl.col('geo.city').str.to_uppercase()",
        ),
        expression_step("bonus", "bonus = pl.col('geo.score') + 1"),
    ];
    // Before any run the chain is accepted: the steps that read the command
    // wait, reading null, whatever their type.
    adapter
        .submit(chain_request(1, 0, &[], &chain, None))
        .unwrap();
    assert!(wait_completion(&mut adapter, 1).await.result.is_ok());
    let rows = wait_page(&mut adapter, 3).await;
    assert!(
        rows.iter().all(|row| {
            row.fields.contains(&("city_upper".into(), "null".into()))
                && row.fields.contains(&("bonus".into(), "null".into()))
        }),
        "unrun command columns read as null: {rows:?}"
    );
    // So is a filter over the unrun command's output: valid, and not applied
    // until results exist, so the command keeps the rows it needs as input.
    let mut waiting = chain_request(2, 1, &chain, &chain, Some("pl.col('geo.score') > 5"));
    waiting.purpose = QueryPurpose::Advanced;
    adapter.submit(waiting).unwrap();
    assert!(wait_completion(&mut adapter, 2).await.result.is_ok());
    let status = adapter.status("view").unwrap();
    assert_eq!(status.matched_records, 3, "{status:?}");
    assert!(
        status
            .diagnostic
            .as_deref()
            .is_some_and(|text| text.contains("waits for a command step that has not run")),
        "{status:?}"
    );
    let mut back = chain_request(3, 2, &chain, &chain, None);
    back.base_constraints.advanced_polars = Some("pl.col('geo.score') > 5".into());
    back.purpose = QueryPurpose::Advanced;
    adapter.submit(back).unwrap();
    assert!(wait_completion(&mut adapter, 3).await.result.is_ok());

    let source_id = handle.source_id().0.to_string();
    let results = std::collections::HashMap::from([
        (
            (source_id.clone(), rows[0].id.sequence),
            BTreeMap::from([
                ("city".to_owned(), serde_json::json!("oslo")),
                ("score".to_owned(), serde_json::json!(7)),
            ]),
        ),
        (
            (source_id.clone(), rows[2].id.sequence),
            BTreeMap::from([
                ("city".to_owned(), serde_json::json!("rome")),
                ("score".to_owned(), serde_json::json!(2)),
            ]),
        ),
    ]);
    assert!(adapter.set_command_results("view", "command-1", "geo", results.clone()));
    assert!(
        !adapter.set_command_results("view", "command-1", "geo", results),
        "an identical publication changes nothing"
    );
    // The app reaffirms the chain once results land: same definition, new
    // revision, and the later step now reads the column.
    adapter
        .submit(chain_request(4, 3, &chain, &chain, None))
        .unwrap();
    assert!(wait_completion(&mut adapter, 4).await.result.is_ok());
    let rows = wait_page(&mut adapter, 3).await;
    assert!(
        rows[0]
            .fields
            .contains(&("city_upper".into(), "OSLO".into())),
        "{rows:?}"
    );
    assert!(
        rows[1]
            .fields
            .contains(&("city_upper".into(), "null".into())),
        "{rows:?}"
    );
    assert!(
        rows[2]
            .fields
            .contains(&("city_upper".into(), "ROME".into())),
        "{rows:?}"
    );

    // A filter over the command's output is an ordinary typed filter.
    let mut filtered = chain_request(5, 4, &chain, &chain, Some("pl.col('geo.score') > 5"));
    filtered.purpose = QueryPurpose::Advanced;
    adapter.submit(filtered).unwrap();
    assert!(wait_completion(&mut adapter, 5).await.result.is_ok());
    let rows = wait_page(&mut adapter, 1).await;
    assert_eq!(rows[0].text, "alpha");

    // A step may not read a command that runs after it: the order is the
    // meaning, and the chain is rejected with the reason, keeping the last one.
    let reversed = vec![chain[1].clone(), chain[0].clone()];
    let mut request = chain_request(6, 5, &chain, &reversed, Some("pl.col('geo.score') > 5"));
    request.base_constraints.advanced_polars = Some("pl.col('geo.score') > 5".into());
    adapter.submit(request).unwrap();
    let failed = wait_completion(&mut adapter, 6).await;
    let error = failed.result.unwrap_err();
    assert_eq!(error.purpose, QueryPurpose::Enrichment);
    assert!(
        error.message.contains("before command step geo"),
        "{}",
        error.message
    );
    assert_eq!(wait_page(&mut adapter, 1).await[0].text, "alpha");

    // A filter over a command step nobody has run is valid and matches nothing.
    let unrun = vec![command_step("command-2", "other")];
    let mut request = chain_request(7, 5, &chain, &unrun, Some("pl.col('other.tag') == 'x'"));
    request.base_constraints.advanced_polars = Some("pl.col('geo.score') > 5".into());
    adapter.submit(request).unwrap();
    assert!(wait_completion(&mut adapter, 7).await.result.is_ok());
    assert!(adapter.clear_command_results("view", "command-1"));
    assert!(!adapter.clear_command_results("view", "command-1"));
    adapter.shutdown();
    manager.shutdown().await;
}

/// A command's input is the steps before it (§12.6): freezing through a step
/// replays only those, with only the command results published before it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn freeze_input_through_a_step_replays_only_what_precedes_it() {
    let root = TempDir::new().unwrap();
    let (manager, handle, mut adapter) = setup(&root, "alpha\nbeta\n", true).await;
    let chain = vec![
        expression_step("first", "a = pl.lit(1)"),
        command_step("command-1", "geo"),
        expression_step("after", "b = pl.col('geo.score') + 1"),
    ];
    adapter
        .submit(chain_request(1, 0, &[], &chain, None))
        .unwrap();
    assert!(wait_completion(&mut adapter, 1).await.result.is_ok());
    let rows = wait_page(&mut adapter, 2).await;
    let source_id = handle.source_id().0.to_string();
    adapter.set_command_results(
        "view",
        "command-1",
        "geo",
        std::collections::HashMap::from([(
            (source_id, rows[0].id.sequence),
            BTreeMap::from([("score".to_owned(), serde_json::json!(4))]),
        )]),
    );
    adapter
        .submit(chain_request(2, 1, &chain, &chain, None))
        .unwrap();
    assert!(wait_completion(&mut adapter, 2).await.result.is_ok());
    let rows = wait_page(&mut adapter, 2).await;
    assert!(
        rows[0].fields.contains(&("b".into(), "5".into())),
        "{rows:?}"
    );

    let fields_of = |frozen: lvu_view::FrozenInput| {
        std::thread::spawn(move || {
            let mut names = std::collections::BTreeSet::new();
            frozen
                .visit(&AtomicBool::new(false), |batch| {
                    for row in batch.rows {
                        names.extend(row.fields.keys().cloned());
                    }
                    Ok(())
                })
                .unwrap();
            names
        })
        .join()
        .unwrap()
    };
    let through = fields_of(
        adapter
            .freeze_input_through("view", Some("command-1"), FrozenInputLimits::default())
            .unwrap(),
    );
    assert!(through.contains("a"), "{through:?}");
    assert!(
        !through.contains("b"),
        "a later step is not the command's input: {through:?}"
    );
    assert!(
        !through.contains("geo.score"),
        "a command never reads its own output: {through:?}"
    );
    let whole = fields_of(
        adapter
            .freeze_input("view", FrozenInputLimits::default())
            .unwrap(),
    );
    assert!(
        whole.contains("b") && whole.contains("geo.score"),
        "{whole:?}"
    );
    assert!(
        adapter
            .freeze_input_through("view", Some("missing"), FrozenInputLimits::default())
            .is_err()
    );
    adapter.shutdown();
    manager.shutdown().await;
}
