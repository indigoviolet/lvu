use lvu::{
    QueryConstraints, QueryPurpose, QueryRequest, TextConstraint, terminal::QueryDispatcher,
};
use lvu_core::{Acquisition, SourceDefinition, SourceId};
use lvu_ingest::{RuntimeConfig, RuntimeState, SourceHandle, SourceManager};
use lvu_live::{LiveConfig, LiveRowProvider};
use lvu_query::CompilerHostConfig;
use lvu_view::{
    AssistancePreparationLimits, AssistancePreparationState, NativeViewAdapter, ViewConfig,
};
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
        name: "assistance fixture".into(),
        acquisition: Acquisition::File {
            path: path.into(),
            follow,
        },
        identity_hints: BTreeMap::new(),
        retention: None,
    }
}

fn runtime_config() -> RuntimeConfig {
    let mut config = RuntimeConfig::default();
    config.acquisition.read_chunk_bytes = 4 * 1024;
    config.batch_records = 32;
    config.max_page_records = 64;
    config.max_page_bytes = 64 * 1024;
    config
}

async fn wait_runtime(handle: &SourceHandle, records: u64) {
    let mut progress = handle.subscribe();
    tokio::time::timeout(Duration::from_secs(5), async {
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

async fn setup(
    root: &TempDir,
    contents: &str,
    follow: bool,
) -> (SourceManager, SourceHandle, NativeViewAdapter) {
    setup_with_compiler(root, contents, follow, false).await
}

async fn setup_with_compiler(
    root: &TempDir,
    contents: &str,
    follow: bool,
    compiler: bool,
) -> (SourceManager, SourceHandle, NativeViewAdapter) {
    let input = root.path().join("input.log");
    fs::write(&input, contents).unwrap();
    let manager = SourceManager::new(root.path().join("capture"), runtime_config()).unwrap();
    let handle = manager
        .start(source(SourceId::new(), &input, follow))
        .await
        .unwrap();
    wait_runtime(&handle, contents.lines().count() as u64).await;
    let mut live = LiveConfig::new(root.path().join("raw-index"));
    live.index_page_records = 32;
    live.index_page_bytes = 64 * 1024;
    live.cache_rows = 64;
    live.cache_bytes = 256 * 1024;
    let mut view = ViewConfig::new(root.path().join("view-index"));
    view.page_records = 32;
    view.page_bytes = 64 * 1024;
    view.compiler = compiler.then(|| CompilerHostConfig {
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
    let raw = Arc::new(LiveRowProvider::new(live).unwrap());
    let adapter = NativeViewAdapter::new(raw, view).unwrap();
    adapter.register_source(lvu_shared::AnySourceHandle::Local(handle.clone())).unwrap();
    adapter
        .register_view("view", vec![handle.source_id()])
        .unwrap();
    (manager, handle, adapter)
}

fn request(text: Option<&str>, enrichments: Vec<lvu::EnrichmentDefinition>) -> QueryRequest {
    QueryRequest {
        view_id: "view".into(),
        generation: 1,
        revision: 1,
        base_revision: 0,
        base_constraints: QueryConstraints::default(),
        purpose: if enrichments.is_empty() {
            QueryPurpose::Search
        } else {
            QueryPurpose::Enrichment
        },
        constraints: QueryConstraints {
            text: text.map(|literal| TextConstraint {
                literal: literal.into(),
                case_insensitive: true,
            }),
            enrichments,
            ..QueryConstraints::default()
        },
    }
}

async fn wait_completion(adapter: &mut NativeViewAdapter) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            adapter.drain_updates(64);
            if let Some(completion) = adapter.poll() {
                assert!(completion.result.is_ok(), "{completion:?}");
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

fn wait_preparation(
    job: &lvu_view::AssistancePreparationJob,
) -> lvu_view::AssistancePreparationStatus {
    let started = std::time::Instant::now();
    loop {
        let status = job.poll();
        if !matches!(
            status.state,
            AssistancePreparationState::Pending | AssistancePreparationState::Running
        ) {
            return status;
        }
        assert!(started.elapsed() < Duration::from_secs(15));
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_preparation_is_inline_typed_stratified_and_excludes_later_arrivals() {
    let root = TempDir::new().unwrap();
    let rows = (0..140)
        .map(|index| {
            format!(
                "{{\"index\":{index},\"unsafe\":18446744073709551615,\"nullable\":{}}}\n",
                if index == 0 { "null" } else { "true" }
            )
        })
        .collect::<String>();
    let (manager, handle, mut adapter) = setup(&root, &rows, true).await;
    let job = adapter
        .start_assistance_preparation(
            "view",
            root.path().join("prepared"),
            AssistancePreparationLimits::default(),
        )
        .unwrap();
    assert!(job.output_dir().is_absolute());
    let mut file = OpenOptions::new()
        .append(true)
        .open(root.path().join("input.log"))
        .unwrap();
    writeln!(file, "{{\"index\":999,\"unsafe\":1}}").unwrap();
    file.flush().unwrap();
    wait_runtime(&handle, 141).await;
    let status = wait_preparation(&job);
    assert_eq!(status.state, AssistancePreparationState::Complete);
    let result = status.result.unwrap();
    assert!(result.context_path.is_absolute());
    assert_eq!(
        result.context_path,
        fs::canonicalize(&result.context_path).unwrap()
    );
    assert!(result.inline_context.len() <= 32 * 1024);
    assert_eq!(
        result.inline_context,
        fs::read_to_string(&result.context_path).unwrap()
    );
    assert!(!job.output_dir().join("filtered").exists());
    assert!(!job.output_dir().join("source").exists());
    assert!(fs::read_dir(job.output_dir()).unwrap().count() == 1);
    let samples = result.context["samples"].as_array().unwrap();
    assert_eq!(samples.first().unwrap()["sequence"], "0");
    assert_eq!(samples.last().unwrap()["sequence"], "139");
    assert!(samples.iter().all(|sample| sample["sequence"] != "140"));
    assert!(samples[0]["values"].get("unsafe").is_none());
    assert!(
        samples[0]["omitted_values"]["unsafe"]
            .as_str()
            .unwrap()
            .contains("lossy float")
    );
    assert!(samples[0]["raw"].is_object());
    assert!(
        result.context["schemas"]
            .as_array()
            .unwrap()
            .iter()
            .any(|schema| {
                schema["field"] == "unsafe"
                    && schema["dtype"] == "Float64"
                    && schema["raw_observed_types"] == serde_json::json!(["uint64"])
                    && schema["projection_loss_observed"] == true
            })
    );
    assert!(samples[0]["values"]["nullable"]["value"].is_null());
    assert_eq!(result.context["sources"][0]["available_rows"], 140);
    assert_eq!(result.applied_revision, 0);
    adapter.shutdown();
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ordinal_sampling_stays_balanced_across_reserved_sequence_gap() {
    let root = TempDir::new().unwrap();
    let early = (0..128)
        .map(|index| format!("ordinal-{index:04}\n"))
        .collect::<String>();
    let (manager, original, mut adapter) = setup(&root, &early, false).await;
    let source_id = original.source_id();
    let _ = original.stop().await;
    adapter.shutdown();

    let mut file = OpenOptions::new()
        .append(true)
        .open(root.path().join("input.log"))
        .unwrap();
    for index in 128..8_192 {
        writeln!(file, "ordinal-{index:04}").unwrap();
    }
    file.flush().unwrap();
    let restarted = manager
        .start(source(source_id, &root.path().join("input.log"), false))
        .await
        .unwrap();
    wait_runtime(&restarted, 8_192).await;
    let high_watermark = restarted.progress().high_watermark.unwrap().sequence;
    assert!(
        high_watermark > 8_192,
        "fixture did not reserve a sequence gap"
    );

    let mut live = LiveConfig::new(root.path().join("gap-raw-index"));
    live.index_page_records = 32;
    live.index_page_bytes = 64 * 1024;
    live.cache_rows = 64;
    live.cache_bytes = 256 * 1024;
    let mut view = ViewConfig::new(root.path().join("gap-view-index"));
    view.page_records = 32;
    view.page_bytes = 64 * 1024;
    view.compiler = None;
    let raw = Arc::new(LiveRowProvider::new(live).unwrap());
    let mut adapter = NativeViewAdapter::new(raw, view).unwrap();
    adapter.register_source(lvu_shared::AnySourceHandle::Local(restarted.clone())).unwrap();
    adapter.register_view("view", vec![source_id]).unwrap();

    let prepared = adapter
        .start_assistance_preparation(
            "view",
            root.path().join("gap-prepared"),
            AssistancePreparationLimits::default(),
        )
        .unwrap()
        .wait();
    assert_eq!(prepared.state, AssistancePreparationState::Complete);
    let context = prepared.result.unwrap().context;
    let samples = context["samples"].as_array().unwrap();
    assert!(samples.len() >= 100);
    let sequences = samples
        .iter()
        .map(|sample| sample["sequence"].as_str().unwrap().parse::<u64>().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(sequences.first(), Some(&0));
    assert_eq!(sequences.last(), Some(&high_watermark));
    let ordinals = samples
        .iter()
        .map(|sample| {
            sample["raw"]["value"]
                .as_str()
                .unwrap()
                .trim()
                .strip_prefix("ordinal-")
                .unwrap()
                .parse::<usize>()
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(ordinals.first(), Some(&0));
    assert_eq!(ordinals.last(), Some(&8_191));
    for quarter in 0..4 {
        assert!(
            ordinals
                .iter()
                .filter(|ordinal| (**ordinal / 2_048).min(3) == quarter)
                .count()
                >= 20,
            "ordinal quarter {quarter} was underrepresented: {ordinals:?}"
        );
    }
    let available = context["sources"][0]["available_rows"].as_u64().unwrap();
    // Not a literal count. Resuming a stopped source replays the records
    // between its last checkpoint and where it actually stopped, so the
    // restarted run ingests the 8,192 lines plus however many the checkpoint
    // lagged by — 8,192, 8,193 or 8,194 here, varying with load. That is the
    // fixture's race, not the sampler's: what this test is about is that the
    // sample stays balanced across the reserved gap, and what it can state
    // exactly is that the export accounts for every record the source
    // ingested and no others. The old `8_192..=8_193` was a guess at the
    // replay window and failed roughly one run in ten under load.
    let ingested = restarted.progress().records;
    assert!(
        ingested >= 8_192,
        "every written line reached the source: {ingested}"
    );
    assert_eq!(
        available, ingested,
        "the export accounts for exactly the records the source ingested"
    );
    assert_eq!(
        context["view"]["scanned_records"].as_u64().unwrap(),
        available * 2
    );
    assert_eq!(
        context["sources"][0]["omitted_rows"].as_u64().unwrap(),
        available - samples.len() as u64
    );
    adapter.shutdown();
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cumulative_budget_can_exhaust_during_selected_ordinal_replay() {
    let root = TempDir::new().unwrap();
    let input = (0..100)
        .map(|index| format!("record-{index}\n"))
        .collect::<String>();
    let (manager, _handle, mut adapter) = setup(&root, &input, false).await;
    let job = adapter
        .start_assistance_preparation(
            "view",
            root.path().join("second-pass-limited"),
            AssistancePreparationLimits {
                maximum_scanned_records: 150,
                ..AssistancePreparationLimits::default()
            },
        )
        .unwrap();
    let output = job.output_dir().to_owned();
    let status = job.wait();
    assert_eq!(status.state, AssistancePreparationState::Limited);
    assert_eq!(status.scanned_records, 100);
    assert!(status.result.is_none());
    assert!(!output.exists());
    adapter.shutdown();
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn schema_evidence_survives_unsampled_rows_and_inline_value_omissions() {
    let root = TempDir::new().unwrap();
    let wide = "x".repeat(1_000);
    let input = (0..300)
        .map(|index| {
            if index == 0 {
                format!("{{\"mix\":\"first-string\",\"wide\":\"{wide}\"}}\n")
            } else if index == 1 {
                format!("{{\"mix\":1,\"rare\":null,\"wide\":\"{wide}\"}}\n")
            } else {
                format!("{{\"mix\":{index},\"wide\":\"{wide}\"}}\n")
            }
        })
        .collect::<String>();
    let (manager, _handle, mut adapter) = setup(&root, &input, false).await;
    let status = adapter
        .start_assistance_preparation(
            "view",
            root.path().join("schema-evidence"),
            AssistancePreparationLimits::default(),
        )
        .unwrap()
        .wait();
    assert_eq!(status.state, AssistancePreparationState::Complete);
    let context = status.result.unwrap().context;
    assert!(
        context["omissions"]["rows_for_inline_byte_limit"] != 0
            || context["omissions"]["values_for_inline_byte_limit"] != 0
    );
    assert!(context["samples"].as_array().unwrap().iter().any(|sample| {
        sample["omitted_values"]["mix"]
            .as_str()
            .is_some_and(|reason| reason.contains("type conflict"))
            && sample["raw"].is_object()
    }));
    assert!(
        context["samples"]
            .as_array()
            .unwrap()
            .iter()
            .all(|sample| sample["sequence"] != "1")
    );
    let schemas = context["schemas"].as_array().unwrap();
    assert!(
        schemas
            .iter()
            .any(|schema| { schema["field"] == "rare" && schema["nullable_observed"] == true })
    );
    assert!(
        schemas
            .iter()
            .any(|schema| { schema["field"] == "rare" && schema["missing_observed"] == true })
    );
    assert!(
        schemas
            .iter()
            .filter(|schema| schema["field"] == "mix")
            .all(|schema| {
                schema["type_conflict_observed"] == true
                    && schema["projection_conflict_observed"] == true
                    && schema["raw_observed_types"] == serde_json::json!(["int64", "string"])
            })
    );
    adapter.shutdown();
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn accepted_native_shadow_uses_derived_integer_not_raw_field() {
    let root = TempDir::new().unwrap();
    let (manager, _handle, mut adapter) =
        setup_with_compiler(&root, "{\"code\":1}\n", false, true).await;
    adapter
        .submit(request(
            None,
            vec![lvu::EnrichmentDefinition {
                id: lvu::EnrichmentStageId("code".into()),
                source: "code = pl.lit(9007199254740993, dtype=pl.Int64)".into(),
                command: None,
            }],
        ))
        .unwrap();
    wait_completion(&mut adapter).await;
    let status = adapter
        .start_assistance_preparation(
            "view",
            root.path().join("derived-shadow"),
            AssistancePreparationLimits::default(),
        )
        .unwrap()
        .wait();
    assert_eq!(status.state, AssistancePreparationState::Complete);
    let context = status.result.unwrap().context;
    assert_eq!(
        context["samples"][0]["values"]["code"]["value"],
        serde_json::json!({"kind":"i64","decimal":"9007199254740993"})
    );
    assert!(context["schemas"].as_array().unwrap().iter().any(|schema| {
        schema["field"] == "code" && schema["provenance"] == "native_enrichment"
    }));
    adapter.shutdown();
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_values_and_zero_match_source_context_are_explicit() {
    let root = TempDir::new().unwrap();
    let (manager, _handle, mut adapter) = setup(
        &root,
        "status=200 name=first\nstatus=500 name=last\n",
        false,
    )
    .await;
    let accepted = request(
        None,
        vec![lvu::EnrichmentDefinition {
            id: lvu::EnrichmentStageId("code".into()),
            source: r"/(?P<code>status=\d+)/".into(),
            command: None,
        }],
    );
    adapter.submit(accepted.clone()).unwrap();
    wait_completion(&mut adapter).await;
    let native = adapter
        .start_assistance_preparation(
            "view",
            root.path().join("native"),
            AssistancePreparationLimits::default(),
        )
        .unwrap()
        .wait();
    assert_eq!(native.state, AssistancePreparationState::Complete);
    let context = native.result.unwrap().context;
    assert!(
        context["samples"]
            .as_array()
            .unwrap()
            .iter()
            .all(|sample| { sample["values"].get("code").is_some() })
    );
    assert!(context["schemas"].as_array().unwrap().iter().any(|schema| {
        schema["field"] == "code" && schema["provenance"] == "native_enrichment"
    }));

    let mut fallback_request = accepted.clone();
    fallback_request.generation = 2;
    fallback_request.revision = 2;
    fallback_request.base_revision = 1;
    fallback_request.base_constraints = accepted.constraints.clone();
    fallback_request.constraints.text = Some(TextConstraint {
        literal: "no record matches this".into(),
        case_insensitive: true,
    });
    fallback_request.purpose = QueryPurpose::Search;
    adapter.submit(fallback_request).unwrap();
    wait_completion(&mut adapter).await;
    let fallback = adapter
        .start_assistance_preparation(
            "view",
            root.path().join("fallback"),
            AssistancePreparationLimits::default(),
        )
        .unwrap()
        .wait();
    let context = fallback.result.unwrap().context;
    assert_eq!(context["sources"][0]["dataset"], "source_context_fallback");
    assert_eq!(context["sources"][0]["available_rows"], 2);
    assert_eq!(context["samples"].as_array().unwrap().len(), 2);
    adapter.shutdown();
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn total_byte_limit_omits_whole_values_and_never_claims_full_coverage() {
    let root = TempDir::new().unwrap();
    let escaped = "é\\\"\n".repeat(2_000);
    let input = format!(
        "{{\"wide\":{}}}\n{{\"wide\":{}}}\n",
        serde_json::to_string(&escaped).unwrap(),
        serde_json::to_string(&escaped).unwrap()
    );
    let (manager, _handle, mut adapter) = setup(&root, &input, false).await;
    let limit = 2_048;
    let status = adapter
        .start_assistance_preparation(
            "view",
            root.path().join("bounded"),
            AssistancePreparationLimits {
                maximum_inline_context_bytes: limit,
                ..AssistancePreparationLimits::default()
            },
        )
        .unwrap()
        .wait();
    assert_eq!(status.state, AssistancePreparationState::Complete);
    let result = status.result.unwrap();
    assert!(result.inline_context.len() <= limit);
    serde_json::from_str::<serde_json::Value>(&result.inline_context).unwrap();
    let coverage = &result.context["sources"][0];
    assert_eq!(coverage["available_rows"], 2);
    assert!(
        coverage["omitted_rows"].as_u64().unwrap() > 0
            || coverage["omitted_values"].as_u64().unwrap() > 0
    );
    assert!(result.context["omissions"]["reason"].is_string());

    let limited = adapter
        .start_assistance_preparation(
            "view",
            root.path().join("impossible"),
            AssistancePreparationLimits {
                maximum_inline_context_bytes: 1,
                ..AssistancePreparationLimits::default()
            },
        )
        .unwrap();
    let impossible_dir = limited.output_dir().to_owned();
    let limited = limited.wait();
    assert_eq!(limited.state, AssistancePreparationState::Limited);
    assert!(limited.result.is_none());
    assert!(!impossible_dir.exists());
    adapter.shutdown();
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancellation_publishes_no_context_and_releases_the_shared_job_lease() {
    let root = TempDir::new().unwrap();
    let input = (0..20_000)
        .map(|index| format!("{{\"index\":{index}}}\n"))
        .collect::<String>();
    let (manager, _handle, mut adapter) = setup(&root, &input, false).await;
    let job = adapter
        .start_assistance_preparation(
            "view",
            root.path().join("cancelled"),
            AssistancePreparationLimits {
                batch_records: 1,
                ..AssistancePreparationLimits::default()
            },
        )
        .unwrap();
    let output = job.output_dir().to_owned();
    job.cancel();
    let status = job.wait();
    assert_eq!(status.state, AssistancePreparationState::Cancelled);
    assert!(status.result.is_none());
    assert!(!output.exists());

    let next = adapter
        .start_assistance_preparation(
            "view",
            root.path().join("after-cancel"),
            AssistancePreparationLimits {
                maximum_scanned_records: 1,
                ..AssistancePreparationLimits::default()
            },
        )
        .unwrap()
        .wait();
    assert_eq!(next.state, AssistancePreparationState::Limited);
    adapter.shutdown();
    manager.shutdown().await;
}
