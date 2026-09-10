use std::sync::atomic::AtomicBool;

use lvu_core::{ChunkPosition, RawRecord, RecordId, SourceId, StreamKind};
use lvu_query::*;
use polars::prelude::*;
use uuid::Uuid;

fn record(source: SourceId, sequence: u64, bytes: &[u8], chunk: ChunkPosition) -> RawRecord {
    RawRecord {
        record_id: RecordId {
            source_id: source,
            sequence,
        },
        captured_at_unix_nanos: sequence as i64,
        stream: StreamKind::File,
        bytes: bytes.into(),
        delimiter: b"\n".into(),
        acquisition_id: Uuid::nil(),
        chunk,
    }
}
fn definition(source: &str, expression: Expr, kind: ExpressionKind) -> CompiledDefinition {
    CompiledDefinition::compile(
        source.into(),
        &serde_json::to_string(&expression).unwrap(),
        kind,
    )
    .unwrap()
}

fn python_compiler() -> CompilerHost {
    let mut config = CompilerHostConfig::python_module("uv", "unused");
    config.args = vec![
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
    ];
    config.timeout = std::time::Duration::from_secs(10);
    CompilerHost::new(config)
}

fn assert_partition_equivalent(frame: &DataFrame, expression: Expr) {
    let whole = frame
        .clone()
        .lazy()
        .select([expression.clone()])
        .collect()
        .unwrap();
    let mut parts = (0..frame.height()).map(|index| {
        frame
            .slice(index as i64, 1)
            .lazy()
            .select([expression.clone()])
            .collect()
            .unwrap()
    });
    let mut partitioned = parts.next().unwrap_or_default();
    for part in parts {
        partitioned.vstack_mut(&part).unwrap();
    }
    assert_eq!(whole.schema(), partitioned.schema());
    assert!(whole.equals_missing(&partitioned));
}

#[test]
fn regex_shorthand_exposes_and_executes_all_named_captures() {
    let plan = parse_regex_enrichment(r"/request_id=(?P<request_id>\S+).*status=(?P<status>\d+)/")
        .unwrap()
        .unwrap();
    assert_eq!(
        plan.outputs()
            .iter()
            .map(|output| (output.name.as_str(), output.capture_index))
            .collect::<Vec<_>>(),
        [("request_id", Some(1)), ("status", Some(2))]
    );
    assert!(plan.outputs()[0].expression_source.contains("str.extract"));

    let source = SourceId::new();
    let records = [
        record(
            source,
            0,
            br"request_id=req/1 status=500",
            ChunkPosition::Complete,
        ),
        record(
            source,
            1,
            b"ordinary unmatched text",
            ChunkPosition::Complete,
        ),
    ];
    let batch = records_to_batch(&records).unwrap();
    let result = execute_batch(
        &batch.frame,
        BatchQuery {
            generation: 1,
            definition_generation: 1,
            stages: plan.stages(),
            filter: None,
            text_search: None,
            colors: &[],
            column_colors: &[],
        },
    );
    assert_eq!(result.validity, BatchValidity::Valid);
    assert_eq!(
        result
            .enriched_rows
            .column("request_id")
            .unwrap()
            .str()
            .unwrap()
            .iter()
            .collect::<Vec<_>>(),
        [Some("req/1"), None]
    );
    assert_eq!(
        result
            .enriched_rows
            .column("status")
            .unwrap()
            .str()
            .unwrap()
            .iter()
            .collect::<Vec<_>>(),
        [Some("500"), None]
    );
    assert_eq!(records[0].bytes, br"request_id=req/1 status=500");
}

#[test]
fn regex_shorthand_is_bounded_and_rejects_ambiguous_outputs() {
    assert!(
        parse_regex_enrichment("value = pl.col('raw')")
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        parse_regex_enrichment(r"/(\d+)/"),
        Err(RegexEnrichmentError::NoNamedCaptures)
    ));
    assert!(matches!(
        parse_regex_enrichment(r"/(?P<raw>.*)/"),
        Err(RegexEnrichmentError::InvalidName(name)) if name == "raw"
    ));
    assert!(matches!(
        parse_regex_enrichment(r"/(?P<ok>.*)/x"),
        Err(RegexEnrichmentError::InvalidFlag('x'))
    ));
    let escaped = parse_regex_enrichment(r"/path=(?P<path>a\/b)/i")
        .unwrap()
        .unwrap();
    assert_eq!(escaped.pattern(), r"(?i:path=(?P<path>a/b))");

    let duplicate = [
        EnrichmentDefinition {
            id: EnrichmentStageId("first".into()),
            source: "/(?P<value>a)/".into(),
        },
        EnrichmentDefinition {
            id: EnrichmentStageId("second".into()),
            source: "/(?P<value>b)/".into(),
        },
    ];
    assert!(matches!(
        compile_enrichment_chain(&duplicate, None, &AtomicBool::new(false)),
        Err(EnrichmentCompileError::DuplicateOutput(name)) if name == "value"
    ));
}

#[test]
fn case_conversion_is_native_row_local_and_preserves_nulls() {
    let frame = df!("raw" => [Some("Hello"), Some("Straße"), Some("ÉTÉ"), Some(""), None]).unwrap();
    for (expression, expected) in [
        (
            col("raw").str().to_uppercase(),
            vec![Some("HELLO"), Some("STRASSE"), Some("ÉTÉ"), Some(""), None],
        ),
        (
            col("raw").str().to_lowercase(),
            vec![Some("hello"), Some("straße"), Some("été"), Some(""), None],
        ),
    ] {
        let compiled = definition("case conversion", expression, ExpressionKind::Enrichment);
        let expression = compiled.expression(ExpressionKind::Enrichment).unwrap();
        let whole = frame
            .clone()
            .lazy()
            .select([expression.clone()])
            .collect()
            .unwrap();
        assert_eq!(
            whole
                .column("raw")
                .unwrap()
                .str()
                .unwrap()
                .iter()
                .collect::<Vec<_>>(),
            expected
        );
        for (i, expected_value) in expected.iter().enumerate() {
            let one = frame
                .slice(i as i64, 1)
                .lazy()
                .select([expression.clone()])
                .collect()
                .unwrap();
            assert_eq!(
                one.column("raw").unwrap().str().unwrap().get(0),
                *expected_value
            );
        }
    }
    assert!(
        deserialize_and_validate(
            &serde_json::to_string(&col("raw").reverse()).unwrap(),
            ExpressionKind::Enrichment
        )
        .is_err()
    );
}

#[test]
fn scalar_projection_preserves_strings_and_formats_only_supported_scalars() {
    let strings = df!(
        SOURCE_ID_COLUMN => ["source", "source"],
        SEQUENCE_COLUMN => [1_u64, 2],
        "value" => ["\"quoted\"", "überlong"]
    )
    .unwrap();
    let projected = scalar_projection(&strings, "value", 5).unwrap();
    assert_eq!(projected[0].1.as_deref(), Some("\"quot"));
    assert_eq!(projected[1].1.as_deref(), Some("über"));

    let booleans = df!(
        SOURCE_ID_COLUMN => ["source"],
        SEQUENCE_COLUMN => [3_u64],
        "value" => [true]
    )
    .unwrap();
    assert_eq!(
        scalar_projection(&booleans, "value", 32).unwrap()[0]
            .1
            .as_deref(),
        Some("true")
    );

    let numbers = df!(
        SOURCE_ID_COLUMN => ["source", "source"],
        SEQUENCE_COLUMN => [4_u64, 5],
        "value" => [Some(42_i64), None]
    )
    .unwrap();
    assert_eq!(
        scalar_projection(&numbers, "value", 32)
            .unwrap()
            .into_iter()
            .map(|(_, value)| value)
            .collect::<Vec<_>>(),
        vec![Some("42".into()), None]
    );

    // Floats render in the canonical text form native colour equality
    // compares — the same Polars cast, not Rust display — so a value copied
    // from display always matches the rule it names.
    let floats = df!(
        SOURCE_ID_COLUMN => ["source", "source", "source", "source"],
        SEQUENCE_COLUMN => [7_u64, 8, 9, 10],
        "value" => [1.0f64, -0.0f64, 42.5f64, 1e21f64]
    )
    .unwrap();
    assert_eq!(
        scalar_projection(&floats, "value", 32)
            .unwrap()
            .into_iter()
            .map(|(_, value)| value)
            .collect::<Vec<_>>(),
        vec![
            Some("1.0".into()),
            Some("-0.0".into()),
            Some("42.5".into()),
            Some("1e+21".into()),
        ]
    );

    let lists = df!(
        SOURCE_ID_COLUMN => ["source"],
        SEQUENCE_COLUMN => [6_u64],
        "value" => [Series::new("item".into(), [1_i64, 2])]
    )
    .unwrap();
    assert!(
        scalar_projection(&lists, "value", 32)
            .unwrap_err()
            .contains("unsupported enrichment output type")
    );
}

#[test]
fn malformed_and_mixed_records_preserve_exact_bytes_ids_and_chunks() {
    let source = SourceId::new();
    let records = vec![
        record(source, 7, b"{\"x\":1,\"mixed\":2}", ChunkPosition::Complete),
        record(source, 8, b"{bad\xff", ChunkPosition::Start),
        record(source, 9, b"x=text mixed=word", ChunkPosition::End),
    ];
    let batch = records_to_batch(&records).unwrap();
    assert_eq!(batch.frame.height(), 3);
    let bytes = batch
        .frame
        .column(RAW_BYTES_COLUMN)
        .unwrap()
        .binary()
        .unwrap();
    for (index, expected) in records.iter().enumerate() {
        assert_eq!(bytes.get(index), Some(expected.bytes.as_slice()));
    }
    assert_eq!(
        batch
            .frame
            .column(SEQUENCE_COLUMN)
            .unwrap()
            .u64()
            .unwrap()
            .into_no_null_iter()
            .collect::<Vec<_>>(),
        vec![7, 8, 9]
    );
    assert_eq!(
        batch
            .frame
            .column("_lvu_chunk")
            .unwrap()
            .str()
            .unwrap()
            .get(1),
        Some("start")
    );
    assert!(
        batch
            .diagnostics
            .iter()
            .any(|item| item.code == "malformed_json")
    );
    assert_eq!(
        batch.frame.column("mixed").unwrap().i64().unwrap().get(0),
        Some(2)
    );
    assert_eq!(
        batch.frame.column("mixed").unwrap().i64().unwrap().get(2),
        None
    );
    assert_eq!(
        batch
            .frame
            .column("_lvu_type_mixed")
            .unwrap()
            .str()
            .unwrap()
            .get(2),
        Some("string")
    );
    assert!(
        batch
            .diagnostics
            .iter()
            .any(|item| item.code == "type_conflict")
    );
}

#[test]
fn enrichments_broadcast_and_fail_independently_without_touching_raw() {
    let source = SourceId::new();
    let records = vec![
        record(source, 1, b"x=1", ChunkPosition::Complete),
        record(source, 2, b"x=2", ChunkPosition::Complete),
    ];
    let input = records_to_batch(&records).unwrap().frame;
    let stages = vec![
        EnrichmentStage {
            name: "constant".into(),
            definition: definition("pl.lit(9)", lit(9_i64), ExpressionKind::Enrichment),
        },
        EnrichmentStage {
            name: "broken".into(),
            definition: definition(
                "pl.col('absent')",
                col("absent"),
                ExpressionKind::Enrichment,
            ),
        },
        EnrichmentStage {
            name: "still_ok".into(),
            definition: definition("pl.lit('yes')", lit("yes"), ExpressionKind::Enrichment),
        },
    ];
    let result = execute_batch(
        &input,
        BatchQuery {
            generation: 2,
            definition_generation: 3,
            stages: &stages,
            filter: None,
            text_search: None,
            colors: &[],
            column_colors: &[],
        },
    );
    assert_eq!(result.enriched_rows.height(), 2);
    assert_eq!(
        result
            .enriched_rows
            .column("constant")
            .unwrap()
            .i32()
            .unwrap()
            .into_no_null_iter()
            .collect::<Vec<_>>(),
        vec![9, 9]
    );
    assert_eq!(
        result
            .enriched_rows
            .column("still_ok")
            .unwrap()
            .str()
            .unwrap()
            .get(1),
        Some("yes")
    );
    assert!(result.enriched_rows.column("broken").is_err());
    assert_eq!(
        result.enriched_rows.column(RAW_BYTES_COLUMN).unwrap(),
        input.column(RAW_BYTES_COLUMN).unwrap()
    );
}

#[test]
fn boolean_null_predicate_is_unmatched_and_non_boolean_is_diagnostic() {
    let source = SourceId::new();
    let input = records_to_batch(&[
        record(source, 1, b"x=yes", ChunkPosition::Complete),
        record(source, 2, b"plain", ChunkPosition::Complete),
    ])
    .unwrap()
    .frame;
    let filter = definition(
        "pl.col('x') == 'yes'",
        col("x").eq(lit("yes")),
        ExpressionKind::Filter,
    );
    let result = execute_batch(
        &input,
        BatchQuery {
            generation: 1,
            definition_generation: 1,
            stages: &[],
            filter: Some(&filter),
            text_search: None,
            colors: &[],
            column_colors: &[],
        },
    );
    assert_eq!(
        result
            .matched_ids
            .iter()
            .map(|id| id.sequence)
            .collect::<Vec<_>>(),
        vec![1]
    );
    let invalid = definition("pl.col('x')", col("x"), ExpressionKind::Filter);
    let invalid_result = execute_batch(
        &input,
        BatchQuery {
            generation: 2,
            definition_generation: 2,
            stages: &[],
            filter: Some(&invalid),
            text_search: None,
            colors: &[],
            column_colors: &[],
        },
    );
    assert!(invalid_result.matched_ids.is_empty());
    assert!(
        invalid_result
            .diagnostics
            .iter()
            .any(|item| item.code == "predicate_not_boolean")
    );
}

#[test]
fn old_generation_cannot_commit_and_cancellation_is_between_batches() {
    let state = QueryGenerationState::new();
    let old = state.begin();
    let current = state.begin();
    assert_ne!(old, current);
    let cancel = QueryCancellation::new();
    cancel.cancel();
    assert!(!execute_bounded_batches(
        &state,
        Vec::<DataFrame>::new(),
        QueryExecution {
            generation: current,
            total_batches: Some(0),
            plan: QueryPlan {
                definition_generation: 1,
                stages: &[],
                filter: None,
                text_search: None,
                colors: &[],
                column_colors: &[]
            },
            cancellation: &cancel,
        },
        &mut BoundedPageSink::new(1),
        |_| {}
    ));
    assert_eq!(state.committed_generation(), None);
}

#[test]
fn actual_python_helper_compiles_then_rust_executes() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let python = manifest.join("../../python");
    let mut host = CompilerHost::new(CompilerHostConfig {
        executable: "uv".into(),
        args: vec![
            "run".into(),
            "--project".into(),
            python.display().to_string(),
            "--locked".into(),
            "python".into(),
            "-m".into(),
            "lvu_expr_helper".into(),
        ],
        request_limit: 64 * 1024,
        output_limit: 384 * 1024,
        stderr_limit: 32 * 1024,
        timeout: std::time::Duration::from_secs(10),
    });
    let compiled = host
        .compile(
            "pl.col('x') == 'yes'",
            ExpressionKind::Filter,
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(compiled.source, "pl.col('x') == 'yes'");
    let source = SourceId::new();
    let input = records_to_batch(&[
        record(source, 4, b"x=yes", ChunkPosition::Complete),
        record(source, 5, b"x=no", ChunkPosition::Complete),
    ])
    .unwrap()
    .frame;
    let output = execute_batch(
        &input,
        BatchQuery {
            generation: 1,
            definition_generation: 1,
            stages: &[],
            filter: Some(&compiled),
            text_search: None,
            colors: &[],
            column_colors: &[],
        },
    );
    assert_eq!(output.matched_ids[0].sequence, 4);
    let timestamp = host.compile(
        "pl.col('ts').str.to_datetime('%+', strict=False).dt.convert_time_zone('UTC').dt.strftime('%Y-%m-%dT%H:%M:%S%.6fZ')",
        ExpressionKind::Enrichment, &AtomicBool::new(false),
    ).unwrap();
    let input = records_to_batch(&[
        record(
            source,
            6,
            b"ts=2026-09-05T15:30:00+02:00",
            ChunkPosition::Complete,
        ),
        record(source, 7, b"ts=invalid", ChunkPosition::Complete),
    ])
    .unwrap()
    .frame;
    let output = execute_batch(
        &input,
        BatchQuery {
            generation: 2,
            definition_generation: 2,
            stages: &[EnrichmentStage {
                name: "timestamp_utc".into(),
                definition: timestamp,
            }],
            filter: None,
            text_search: None,
            colors: &[],
            column_colors: &[],
        },
    );
    let timestamps = output
        .enriched_rows
        .column("timestamp_utc")
        .unwrap()
        .str()
        .unwrap();
    assert_eq!(timestamps.get(0), Some("2026-09-05T13:30:00.000000Z"));
    assert_eq!(timestamps.get(1), None);
}

#[test]
fn legal_expression_matches_whole_and_partitioned_batches() {
    let source = SourceId::new();
    let records = [
        record(source, 1, b"x=yes", ChunkPosition::Complete),
        record(source, 2, b"x=no", ChunkPosition::Complete),
        record(source, 3, b"x=yes", ChunkPosition::Complete),
    ];
    let filter = definition(
        "pl.col('x') == 'yes'",
        col("x").eq(lit("yes")),
        ExpressionKind::Filter,
    );
    let whole = records_to_batch(&records).unwrap().frame;
    let whole_result = execute_batch(
        &whole,
        BatchQuery {
            generation: 1,
            definition_generation: 1,
            stages: &[],
            filter: Some(&filter),
            text_search: None,
            colors: &[],
            column_colors: &[],
        },
    );
    let mut partitioned = Vec::new();
    for record in &records {
        let batch = records_to_batch(std::slice::from_ref(record))
            .unwrap()
            .frame;
        partitioned.extend(
            execute_batch(
                &batch,
                BatchQuery {
                    generation: 1,
                    definition_generation: 1,
                    stages: &[],
                    filter: Some(&filter),
                    text_search: None,
                    colors: &[],
                    column_colors: &[],
                },
            )
            .matched_ids,
        );
    }
    assert_eq!(whole_result.matched_ids, partitioned);
}

#[test]
fn typed_json_schema_survives_partitioning_nulls_and_conflicts() {
    let source = SourceId::new();
    let numeric = record(
        source,
        1,
        br#"{"status":500,"ok":true,"missing":null}"#,
        ChunkPosition::Complete,
    );
    let absent = record(source, 2, br#"{"ok":false}"#, ChunkPosition::Complete);
    let conflict = record(source, 3, br#"{"status":"oops"}"#, ChunkPosition::Complete);
    let numeric_frame = records_to_batch(std::slice::from_ref(&numeric))
        .unwrap()
        .frame;
    assert_eq!(
        numeric_frame
            .column("status")
            .unwrap()
            .i64()
            .unwrap()
            .get(0),
        Some(500)
    );
    assert_eq!(
        numeric_frame.column("ok").unwrap().bool().unwrap().get(0),
        Some(true)
    );
    assert_eq!(numeric_frame.column("missing").unwrap().null_count(), 1);
    let filter = definition(
        "pl.col('status') >= 500",
        col("status").gt_eq(lit(500_i64)),
        ExpressionKind::Filter,
    );
    assert_eq!(
        execute_batch(
            &numeric_frame,
            BatchQuery {
                generation: 1,
                definition_generation: 1,
                stages: &[],
                filter: Some(&filter),
                text_search: None,
                colors: &[],
                column_colors: &[]
            }
        )
        .matched_ids
        .len(),
        1
    );

    let mut schema = SchemaContext::default();
    let _ = records_to_batch_with_context(std::slice::from_ref(&numeric), &mut schema).unwrap();
    let absent_frame = records_to_batch_with_context(std::slice::from_ref(&absent), &mut schema)
        .unwrap()
        .frame;
    assert!(absent_frame.column("status").is_ok());
    assert_eq!(absent_frame.column("status").unwrap().null_count(), 1);
    let conflict_frame =
        records_to_batch_with_context(std::slice::from_ref(&conflict), &mut schema)
            .unwrap()
            .frame;
    assert_eq!(
        conflict_frame
            .column("status")
            .unwrap()
            .i64()
            .unwrap()
            .get(0),
        None
    );
    assert_eq!(
        conflict_frame
            .column("_lvu_type_status")
            .unwrap()
            .str()
            .unwrap()
            .get(0),
        Some("string")
    );
}

#[test]
fn quoted_logfmt_is_not_split_into_fake_assignments() {
    let source = SourceId::new();
    let batch = records_to_batch(&[record(
        source,
        1,
        br#"message="hello fake=yes world" level=info"#,
        ChunkPosition::Complete,
    )])
    .unwrap();
    assert_eq!(
        batch.frame.column("message").unwrap().str().unwrap().get(0),
        Some("hello fake=yes world")
    );
    assert!(batch.frame.column("fake").is_err());
}

#[test]
fn null_or_duplicate_identity_invalidates_batch_without_losing_rows() {
    let frame = df![SOURCE_ID_COLUMN => [None, Some("s")], SEQUENCE_COLUMN => [1_u64, 2], "flag" => [false, true]].unwrap();
    let filter = definition("pl.col('flag')", col("flag"), ExpressionKind::Filter);
    let output = execute_batch(
        &frame,
        BatchQuery {
            generation: 1,
            definition_generation: 1,
            stages: &[],
            filter: Some(&filter),
            text_search: None,
            colors: &[],
            column_colors: &[],
        },
    );
    assert_eq!(output.validity, BatchValidity::InvalidIdentity);
    assert_eq!(output.enriched_rows.height(), 2);
    assert!(
        output
            .diagnostics
            .iter()
            .any(|item| item.code == "invalid_identity")
    );
}

#[test]
fn failed_overwrite_fences_ast_dependents_but_not_literals() {
    let source = SourceId::new();
    let input = records_to_batch(&[record(source, 1, b"x=old", ChunkPosition::Complete)])
        .unwrap()
        .frame;
    let stages = [
        EnrichmentStage {
            name: "x".into(),
            definition: definition(
                "pl.col('absent')",
                col("absent"),
                ExpressionKind::Enrichment,
            ),
        },
        EnrichmentStage {
            name: "dependent".into(),
            definition: definition("pl.col('x')", col("x"), ExpressionKind::Enrichment),
        },
        EnrichmentStage {
            name: "literal".into(),
            definition: definition("pl.lit('x')", lit("x"), ExpressionKind::Enrichment),
        },
    ];
    let filter = definition(
        "pl.col('x') == 'old'",
        col("x").eq(lit("old")),
        ExpressionKind::Filter,
    );
    let output = execute_batch(
        &input,
        BatchQuery {
            generation: 1,
            definition_generation: 1,
            stages: &stages,
            filter: Some(&filter),
            text_search: None,
            colors: &[],
            column_colors: &[],
        },
    );
    assert_eq!(output.validity, BatchValidity::InvalidFilter);
    assert!(output.enriched_rows.column("dependent").is_err());
    assert_eq!(
        output
            .enriched_rows
            .column("literal")
            .unwrap()
            .str()
            .unwrap()
            .get(0),
        Some("x")
    );
    assert!(
        output
            .diagnostics
            .iter()
            .filter(|item| item.code == "dependency_unavailable")
            .count()
            >= 2
    );
}

#[test]
fn final_progress_cancellation_keeps_prior_pages_and_sink_is_bounded() {
    let state = QueryGenerationState::new();
    let source = SourceId::new();
    let frame = records_to_batch(&[record(source, 1, b"x=yes", ChunkPosition::Complete)])
        .unwrap()
        .frame;
    let mut sink = BoundedPageSink::new(1);
    let first = state.begin();
    assert!(execute_bounded_batches(
        &state,
        vec![frame.clone(), frame.clone()],
        QueryExecution {
            generation: first,
            total_batches: Some(2),
            plan: QueryPlan {
                definition_generation: 1,
                stages: &[],
                filter: None,
                text_search: None,
                colors: &[],
                column_colors: &[]
            },
            cancellation: &QueryCancellation::new(),
        },
        &mut sink,
        |_| {}
    ));
    assert_eq!(sink.published().len(), 1);
    let second = state.begin();
    let cancel = QueryCancellation::new();
    let trigger = cancel.clone();
    assert!(!execute_bounded_batches(
        &state,
        vec![frame],
        QueryExecution {
            generation: second,
            total_batches: Some(1),
            plan: QueryPlan {
                definition_generation: 2,
                stages: &[],
                filter: None,
                text_search: None,
                colors: &[],
                column_colors: &[]
            },
            cancellation: &cancel,
        },
        &mut sink,
        |_| trigger.cancel()
    ));
    assert_eq!(state.committed_generation(), Some(first));
    assert_eq!(sink.published().len(), 1);
    let invalid = definition("pl.col('x')", col("x"), ExpressionKind::Filter);
    let third = state.begin();
    assert!(!execute_bounded_batches(
        &state,
        vec![
            records_to_batch(&[record(source, 2, b"x=no", ChunkPosition::Complete)])
                .unwrap()
                .frame
        ],
        QueryExecution {
            generation: third,
            total_batches: Some(1),
            plan: QueryPlan {
                definition_generation: 3,
                stages: &[],
                filter: Some(&invalid),
                text_search: None,
                colors: &[],
                column_colors: &[]
            },
            cancellation: &QueryCancellation::new()
        },
        &mut sink,
        |_| {},
    ));
    assert_eq!(state.committed_generation(), Some(first));
    assert_eq!(sink.published().len(), 1);
}

#[test]
fn literal_unicode_text_search_combines_with_advanced_filter() {
    let source = SourceId::new();
    let input = records_to_batch(&[
        record(
            source,
            1,
            "ÉCHEC [a.*] status=500".as_bytes(),
            ChunkPosition::Complete,
        ),
        record(
            source,
            2,
            "échec [a.*] status=200".as_bytes(),
            ChunkPosition::Complete,
        ),
        record(
            source,
            3,
            "other status=500".as_bytes(),
            ChunkPosition::Complete,
        ),
    ])
    .unwrap()
    .frame;
    let punctuation = TextSearch::new("échec [A.*]").unwrap();
    let search_only = execute_batch(
        &input,
        BatchQuery {
            generation: 1,
            definition_generation: 1,
            stages: &[],
            filter: None,
            text_search: Some(&punctuation),
            colors: &[],
            column_colors: &[],
        },
    );
    assert_eq!(search_only.matched_ids.len(), 2);
    let advanced = definition(
        "pl.col('status') == '500'",
        col("status").eq(lit("500")),
        ExpressionKind::Filter,
    );
    let combined = execute_batch(
        &input,
        BatchQuery {
            generation: 1,
            definition_generation: 1,
            stages: &[],
            filter: Some(&advanced),
            text_search: Some(&punctuation),
            colors: &[],
            column_colors: &[],
        },
    );
    assert_eq!(
        combined
            .matched_ids
            .iter()
            .map(|id| id.sequence)
            .collect::<Vec<_>>(),
        vec![1]
    );
    let none = execute_batch(
        &input,
        BatchQuery {
            generation: 1,
            definition_generation: 1,
            stages: &[],
            filter: None,
            text_search: Some(&TextSearch::new("not present").unwrap()),
            colors: &[],
            column_colors: &[],
        },
    );
    assert_eq!(none.validity, BatchValidity::Valid);
    assert!(none.matched_ids.is_empty());
    let empty = execute_batch(
        &input,
        BatchQuery {
            generation: 1,
            definition_generation: 1,
            stages: &[],
            filter: None,
            text_search: Some(&TextSearch::new("").unwrap()),
            colors: &[],
            column_colors: &[],
        },
    );
    assert_eq!(empty.matched_ids.len(), 3);
    let invalid = definition("pl.col('status')", col("status"), ExpressionKind::Filter);
    let bad = execute_batch(
        &input,
        BatchQuery {
            generation: 1,
            definition_generation: 1,
            stages: &[],
            filter: Some(&invalid),
            text_search: Some(&punctuation),
            colors: &[],
            column_colors: &[],
        },
    );
    assert_eq!(bad.validity, BatchValidity::InvalidFilter);
}

#[test]
fn literal_fast_path_is_identical_to_lowercase_over_ascii_and_unicode_batches() {
    let source = SourceId::new();
    // This includes the soak's accented/CJK shapes plus the cases that prove an
    // ASCII needle alone is not enough to select the regex path. In particular,
    // lowercasing `İ` produces `i` + combining dot, while Unicode simple folding
    // does not make it equal to `i`.
    let values = [
        "SOAK_MARKER ascii",
        "soak_marker lower",
        "CAFÉ 東京",
        "cafe\u{301} decomposed",
        "İstanbul",
        "ſervice",
        "Kelvin",
        "ΑΣ ς σ",
        "malformed [a.*] punctuation",
    ];
    let records = values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            record(
                source,
                index as u64,
                value.as_bytes(),
                ChunkPosition::Complete,
            )
        })
        .collect::<Vec<_>>();
    let frame = records_to_batch(&records).unwrap().frame;

    for needle in ["soak_marker", "CAFÉ", "東京", "i", "s", "k", "σ", "[A.*]"] {
        let search = TextSearch::new(needle).unwrap();
        let colors = [("same predicate".into(), search.clone())];
        let actual = execute_batch(
            &frame,
            BatchQuery {
                generation: 1,
                definition_generation: 1,
                stages: &[],
                filter: None,
                text_search: Some(&search),
                colors: &colors,
                column_colors: &[],
            },
        );
        let legacy = definition(
            "legacy lowercase literal",
            col(lvu_query::RAW_COLUMN)
                .str()
                .to_lowercase()
                .str()
                .contains_literal(lit(needle.to_lowercase())),
            ExpressionKind::Filter,
        );
        let expected = execute_batch(
            &frame,
            BatchQuery {
                generation: 1,
                definition_generation: 1,
                stages: &[],
                filter: Some(&legacy),
                text_search: None,
                colors: &[],
                column_colors: &[],
            },
        );
        assert_eq!(
            actual.matched_ids, expected.matched_ids,
            "needle {needle:?}"
        );
        assert_eq!(
            actual.color_matches.get("same predicate"),
            Some(&expected.matched_ids),
            "colour needle {needle:?}"
        );
        assert!(actual.color_diagnostics.is_empty(), "needle {needle:?}");
    }
}

#[test]
fn polars_case_insensitive_regex_uses_unicode_simple_folding_not_lowercase_substrings() {
    let frame =
        df!("raw" => ["Σ", "ς", "σ", "ſ", "s", "K", "k", "İ", "i", "é", "e\u{301}"]).unwrap();
    let matches = |pattern: &str| {
        frame
            .clone()
            .lazy()
            .select([col("raw").str().contains(lit(pattern), true).alias("hit")])
            .collect()
            .unwrap()
            .column("hit")
            .unwrap()
            .bool()
            .unwrap()
            .iter()
            .map(|value| value.unwrap_or(false))
            .collect::<Vec<_>>()
    };

    assert_eq!(
        matches("(?i)σ"),
        vec![
            true, true, true, false, false, false, false, false, false, false, false
        ]
    );
    assert_eq!(
        matches("(?i)s"),
        vec![
            false, false, false, true, true, false, false, false, false, false, false
        ]
    );
    assert_eq!(
        matches("(?i)k"),
        vec![
            false, false, false, false, false, true, true, false, false, false, false
        ]
    );
    assert_eq!(
        matches("(?i)i"),
        vec![
            false, false, false, false, false, false, false, false, true, false, false
        ]
    );
    assert_eq!(
        matches("(?i)é"),
        vec![
            false, false, false, false, false, false, false, false, false, true, false
        ]
    );
}

#[test]
fn reversed_type_conflicts_never_coerce_scalars_to_strings() {
    let source = SourceId::new();
    let strings_first = records_to_batch(&[
        record(source, 1, br#"{"value":"hello"}"#, ChunkPosition::Complete),
        record(source, 2, br#"{"value":123}"#, ChunkPosition::Complete),
        record(source, 3, br#"{"value":true}"#, ChunkPosition::Complete),
    ])
    .unwrap();
    let values = strings_first.frame.column("value").unwrap().str().unwrap();
    assert_eq!(values.get(0), Some("hello"));
    assert_eq!(values.get(1), None);
    assert_eq!(values.get(2), None);
    let types = strings_first
        .frame
        .column("_lvu_type_value")
        .unwrap()
        .str()
        .unwrap();
    assert_eq!(
        (types.get(0), types.get(1), types.get(2)),
        (Some("string"), Some("int64"), Some("bool"))
    );

    let bool_first = records_to_batch(&[
        record(source, 4, br#"{"value":true}"#, ChunkPosition::Complete),
        record(source, 5, br#"{"value":"true"}"#, ChunkPosition::Complete),
    ])
    .unwrap();
    assert_eq!(
        bool_first
            .frame
            .column("value")
            .unwrap()
            .bool()
            .unwrap()
            .get(0),
        Some(true)
    );
    assert_eq!(
        bool_first
            .frame
            .column("value")
            .unwrap()
            .bool()
            .unwrap()
            .get(1),
        None
    );
}

#[test]
fn initially_null_numeric_predicate_is_valid_across_partitions() {
    let source = SourceId::new();
    let records = [
        record(source, 1, br#"{"status":null}"#, ChunkPosition::Complete),
        record(source, 2, br#"{"status":500}"#, ChunkPosition::Complete),
    ];
    let filter = definition(
        "pl.col('status') >= 500",
        col("status").gt_eq(lit(500_i64)),
        ExpressionKind::Filter,
    );
    let first = records_to_batch(std::slice::from_ref(&records[0])).unwrap();
    assert_eq!(
        first.frame.column("status").unwrap().dtype(),
        &DataType::Null
    );
    let first_result = execute_batch(
        &first.frame,
        BatchQuery {
            generation: 1,
            definition_generation: 1,
            stages: &[],
            filter: Some(&filter),
            text_search: None,
            colors: &[],
            column_colors: &[],
        },
    );
    assert_eq!(
        first_result.validity,
        BatchValidity::Valid,
        "{:?}",
        first_result.diagnostics
    );
    assert!(first_result.matched_ids.is_empty());

    let whole = records_to_batch(&records).unwrap();
    let whole_matches = execute_batch(
        &whole.frame,
        BatchQuery {
            generation: 1,
            definition_generation: 1,
            stages: &[],
            filter: Some(&filter),
            text_search: None,
            colors: &[],
            column_colors: &[],
        },
    )
    .matched_ids;
    let mut context = SchemaContext::default();
    let left =
        records_to_batch_with_context(std::slice::from_ref(&records[0]), &mut context).unwrap();
    let right =
        records_to_batch_with_context(std::slice::from_ref(&records[1]), &mut context).unwrap();
    let mut partitioned = execute_batch(
        &left.frame,
        BatchQuery {
            generation: 1,
            definition_generation: 1,
            stages: &[],
            filter: Some(&filter),
            text_search: None,
            colors: &[],
            column_colors: &[],
        },
    )
    .matched_ids;
    partitioned.extend(
        execute_batch(
            &right.frame,
            BatchQuery {
                generation: 1,
                definition_generation: 1,
                stages: &[],
                filter: Some(&filter),
                text_search: None,
                colors: &[],
                column_colors: &[],
            },
        )
        .matched_ids,
    );
    assert_eq!(whole_matches, partitioned);
}

#[test]
fn failed_overwrite_fences_color_dependency() {
    let source = SourceId::new();
    let input = records_to_batch(&[record(source, 1, b"x=old", ChunkPosition::Complete)])
        .unwrap()
        .frame;
    let stages = [EnrichmentStage {
        name: "x".into(),
        definition: definition(
            "pl.col('missing')",
            col("missing"),
            ExpressionKind::Enrichment,
        ),
    }];
    let colors = [(
        "stale".into(),
        TextSearch::parse(
            "pl.col('x') == 'old'".into(),
            Some(&definition(
                "pl.col('x') == 'old'",
                col("x").eq(lit("old")),
                ExpressionKind::Filter,
            )),
        )
        .expect("rule predicate"),
    )];
    let output = execute_batch(
        &input,
        BatchQuery {
            generation: 1,
            definition_generation: 1,
            stages: &stages,
            filter: None,
            text_search: None,
            colors: &colors,
            column_colors: &[],
        },
    );
    assert!(!output.color_matches.contains_key("stale"));
    assert!(output.color_diagnostics.iter().any(
        |item| item.field.as_deref() == Some("stale") && item.code == "dependency_unavailable"
    ));
}

#[test]
fn numeric_enrichment_names_never_collide_with_colour_diagnostics() {
    let source = SourceId::new();
    let input = records_to_batch(&[record(source, 1, b"ordinary", ChunkPosition::Complete)])
        .unwrap()
        .frame;
    let color = TextSearch::parse("ordinary".into(), None).unwrap();

    for colors in [&[][..], &[("0".into(), color.clone())][..]] {
        let successful = [EnrichmentStage {
            name: "0".into(),
            definition: definition("pl.lit('ok')", lit("ok"), ExpressionKind::Enrichment),
        }];
        let output = execute_batch(
            &input,
            BatchQuery {
                generation: 1,
                definition_generation: 1,
                stages: &successful,
                filter: None,
                text_search: None,
                colors,
                column_colors: &[],
            },
        );
        assert!(output.color_diagnostics.is_empty());
        assert!(output.diagnostics.iter().any(|item| {
            item.field.as_deref() == Some("0") && item.state == DerivedState::Ready
        }));

        let failing = [EnrichmentStage {
            name: "0".into(),
            definition: definition(
                "pl.col('missing')",
                col("missing"),
                ExpressionKind::Enrichment,
            ),
        }];
        let output = execute_batch(
            &input,
            BatchQuery {
                generation: 1,
                definition_generation: 1,
                stages: &failing,
                filter: None,
                text_search: None,
                colors,
                column_colors: &[],
            },
        );
        assert!(output.color_diagnostics.is_empty());
        assert!(output.diagnostics.iter().any(|item| {
            item.field.as_deref() == Some("0") && item.state == DerivedState::Error
        }));
    }
}

#[test]
fn search_box_field_regex_and_polars_forms_preserve_literal_default() {
    let source = SourceId::new();
    let input = records_to_batch(&[
        record(
            source,
            0,
            br#"{"level":"ERROR","status":503,"message":"Timeout / retry"}"#,
            ChunkPosition::Complete,
        ),
        record(
            source,
            1,
            br#"{"level":"info","status":200,"message":"ready"}"#,
            ChunkPosition::Complete,
        ),
        record(source, 2, br#"{"message":null}"#, ChunkPosition::Complete),
    ])
    .unwrap()
    .frame;
    let compiled = definition(
        "pl.col('status') >= 500",
        col("status").gt_eq(lit(500i64)),
        ExpressionKind::Filter,
    );
    for (source, expected) in [
        ("timeout", vec![0]),
        ("level: error", vec![0]),
        ("status: 50", vec![0]),
        ("/timeout/i", vec![0]),
        (r"message: /Timeout \/ retry/", vec![0]),
        ("message: /^ready$/", vec![1]),
        ("missing: anything", vec![]),
        ("/Timeout/", vec![0]),
        ("/timeout/", vec![]),
        ("pl.col('status') >= 500", vec![0]),
        ("[a.*]", vec![]),
    ] {
        let search = TextSearch::parse(source.into(), Some(&compiled)).unwrap();
        let result = execute_batch(
            &input,
            BatchQuery {
                generation: 1,
                definition_generation: 1,
                stages: &[],
                filter: None,
                text_search: Some(&search),
                colors: &[],
                column_colors: &[],
            },
        );
        assert_eq!(
            result.validity,
            BatchValidity::Valid,
            "{source}: {:?}",
            result.diagnostics
        );
        assert_eq!(
            result
                .matched_ids
                .iter()
                .map(|id| id.sequence)
                .collect::<Vec<_>>(),
            expected,
            "{source}"
        );
    }
    for invalid in ["/unfinished", "/[/", "/a/z", "/a/ii", "message: /(?=x)/"] {
        assert!(
            TextSearch::parse(invalid.into(), None).is_err(),
            "{invalid}"
        );
    }
}

#[test]
fn quoted_field_search_uses_json_names_and_explicit_literal_slash() {
    let source_id = SourceId::new();
    let input = records_to_batch(&[
        record(source_id, 0, br#"{"field name":"/var/log","quote\"field":"OK","colon: field":"Ready","":"empty key"}"#, ChunkPosition::Complete),
        record(source_id, 1, "{\"field name\":\"other\",\"城市\":\"Paris\"}".as_bytes(), ChunkPosition::Complete),
    ]).unwrap().frame;
    for (source, expected) in [
        (r#""field name": \/var/log"#, vec![0]),
        (r#""field name": /^other$/"#, vec![1]),
        (r#""quote\"field": ok"#, vec![0]),
        (r#""colon: field": ready"#, vec![0]),
        (r#""城市": paris"#, vec![1]),
        (r#""": empty"#, vec![0]),
        (r"\/var/log", vec![0]),
        (r#""absent field": x"#, vec![]),
    ] {
        let search = TextSearch::parse(source.into(), None).unwrap();
        let result = execute_batch(
            &input,
            BatchQuery {
                generation: 1,
                definition_generation: 1,
                stages: &[],
                filter: None,
                text_search: Some(&search),
                colors: &[],
                column_colors: &[],
            },
        );
        assert_eq!(
            result.validity,
            BatchValidity::Valid,
            "{source}: {:?}",
            result.diagnostics
        );
        assert_eq!(
            result
                .matched_ids
                .iter()
                .map(|id| id.sequence)
                .collect::<Vec<_>>(),
            expected,
            "{source}"
        );
    }
}

#[test]
fn python_replacements_and_datetime_constructors_execute_in_rust() {
    let mut host = python_compiler();
    let epochs = df!("raw" => [Some(0_i64), Some(1000_i64), None]).unwrap();
    for dtype in [
        "pl.Datetime('ms', 'UTC')",
        "pl.Datetime(time_unit='ms', time_zone='UTC')",
    ] {
        let source = format!("pl.col('raw').cast({dtype}).dt.strftime('%Y-%m-%dT%H:%M:%S%.6fZ')");
        let compiled = host
            .compile(&source, ExpressionKind::Enrichment, &AtomicBool::new(false))
            .unwrap();
        let result = epochs
            .clone()
            .lazy()
            .select([compiled.expression(ExpressionKind::Enrichment).unwrap()])
            .collect()
            .unwrap();
        assert_eq!(
            result
                .column("raw")
                .unwrap()
                .str()
                .unwrap()
                .iter()
                .collect::<Vec<_>>(),
            [
                Some("1970-01-01T00:00:00.000000Z"),
                Some("1970-01-01T00:00:01.000000Z"),
                None
            ]
        );
    }
    let frame = df!("raw" => [Some("a a"), Some("été"), None]).unwrap();
    for (source, expected) in [
        (
            r#"pl.col('raw').str.replace('a', 'X')"#,
            vec![Some("X a"), Some("été"), None],
        ),
        (
            r#"pl.col('raw').str.replace_all('a', 'X')"#,
            vec![Some("X X"), Some("été"), None],
        ),
        (
            r#"pl.col('raw').str.replace_all('(?P<letter>a)', '${letter}!')"#,
            vec![Some("a! a!"), Some("été"), None],
        ),
        (
            r#"pl.col('raw').str.replace('.', '$', literal=True)"#,
            vec![Some("a a"), Some("été"), None],
        ),
    ] {
        let compiled = host
            .compile(source, ExpressionKind::Enrichment, &AtomicBool::new(false))
            .unwrap();
        let expr = compiled.expression(ExpressionKind::Enrichment).unwrap();
        let result = frame
            .clone()
            .lazy()
            .select([expr.clone()])
            .collect()
            .unwrap();
        assert_eq!(
            result
                .column("raw")
                .unwrap()
                .str()
                .unwrap()
                .iter()
                .collect::<Vec<_>>(),
            expected
        );
        for (i, value) in expected.iter().enumerate() {
            let one = frame
                .slice(i as i64, 1)
                .lazy()
                .select([expr.clone()])
                .collect()
                .unwrap();
            assert_eq!(one.column("raw").unwrap().str().unwrap().get(0), *value);
        }
    }
}

#[test]
fn python_broad_row_local_functions_execute_partition_equivalently_in_rust() {
    let mut host = python_compiler();
    let strings = df!("raw" => [Some(" hello "), Some("éclair "), Some(""), None]).unwrap();
    for (source, expected) in [
        (
            r#"pl.col('raw').str.to_uppercase()"#,
            vec![Some(" HELLO "), Some("ÉCLAIR "), Some(""), None],
        ),
        (
            r#"pl.col('raw').str.strip_chars()"#,
            vec![Some("hello"), Some("éclair"), Some(""), None],
        ),
        (
            r#"pl.col('raw').str.slice(1, 3)"#,
            vec![Some("hel"), Some("cla"), Some(""), None],
        ),
        (
            r#"pl.col('raw').str.split('l').list.get(-1, null_on_oob=True)"#,
            vec![Some("o "), Some("air "), Some(""), None],
        ),
    ] {
        let compiled = host
            .compile(source, ExpressionKind::Enrichment, &AtomicBool::new(false))
            .unwrap();
        let expression = compiled.expression(ExpressionKind::Enrichment).unwrap();
        validate_expression_for_frame(&strings, expression.clone()).unwrap();
        let result = strings
            .clone()
            .lazy()
            .select([expression.clone()])
            .collect()
            .unwrap();
        assert_eq!(result.column("raw").unwrap().dtype(), &DataType::String);
        assert_eq!(
            result
                .column("raw")
                .unwrap()
                .str()
                .unwrap()
                .iter()
                .collect::<Vec<_>>(),
            expected
        );
        assert_partition_equivalent(&strings, expression);
    }
    for (source, expected) in [
        (
            r#"pl.col('raw').str.starts_with(' h')"#,
            vec![Some(true), Some(false), Some(false), None],
        ),
        (
            r#"pl.col('raw').str.ends_with(' ')"#,
            vec![Some(true), Some(true), Some(false), None],
        ),
    ] {
        let compiled = host
            .compile(source, ExpressionKind::Enrichment, &AtomicBool::new(false))
            .unwrap();
        let expression = compiled.expression(ExpressionKind::Enrichment).unwrap();
        let result = strings
            .clone()
            .lazy()
            .select([expression.clone()])
            .collect()
            .unwrap();
        assert_eq!(result.column("raw").unwrap().dtype(), &DataType::Boolean);
        assert_eq!(
            result
                .column("raw")
                .unwrap()
                .bool()
                .unwrap()
                .iter()
                .collect::<Vec<_>>(),
            expected
        );
        assert_partition_equivalent(&strings, expression);
    }
    let compiled = host
        .compile(
            r#"pl.col('raw').str.len_chars()"#,
            ExpressionKind::Enrichment,
            &AtomicBool::new(false),
        )
        .unwrap();
    let expression = compiled.expression(ExpressionKind::Enrichment).unwrap();
    let lengths = strings
        .clone()
        .lazy()
        .select([expression.clone()])
        .collect()
        .unwrap();
    assert_eq!(lengths.column("raw").unwrap().dtype(), &DataType::UInt32);
    assert_eq!(
        lengths
            .column("raw")
            .unwrap()
            .u32()
            .unwrap()
            .iter()
            .collect::<Vec<_>>(),
        [Some(7), Some(7), Some(0), None]
    );
    assert_partition_equivalent(&strings, expression);

    let epochs = df!("epoch" => [Some(0_i64), None, Some(1000_i64)]).unwrap();
    let compiled = host
        .compile(
            r#"pl.coalesce(pl.from_epoch(pl.col('epoch'), time_unit='ms'), pl.lit(None, dtype=pl.Datetime('ms')))"#,
            ExpressionKind::Enrichment,
            &AtomicBool::new(false),
        )
        .unwrap();
    let expression = compiled.expression(ExpressionKind::Enrichment).unwrap();
    validate_expression_for_frame(&epochs, expression.clone()).unwrap();
    let result = epochs
        .clone()
        .lazy()
        .select([expression.clone()])
        .collect()
        .unwrap();
    assert_eq!(
        result.column("epoch").unwrap().dtype(),
        &DataType::Datetime(TimeUnit::Milliseconds, None)
    );
    assert_eq!(
        result
            .column("epoch")
            .unwrap()
            .cast(&DataType::Int64)
            .unwrap()
            .i64()
            .unwrap()
            .iter()
            .collect::<Vec<_>>(),
        [Some(0), None, Some(1000)]
    );
    assert_partition_equivalent(&epochs, expression);
}

#[test]
fn python_structural_expressions_are_rejected_by_lowered_plan_metadata() {
    let mut host = python_compiler();
    for (source, expected) in [
        (r#"pl.col('x').shift(1)"#, "row-local"),
        (r#"pl.col('x').reverse()"#, "row-local"),
        (r#"pl.col('x').cum_sum()"#, "deserialization"),
        (r#"pl.col('x').fill_null(strategy='forward')"#, "row-local"),
        (r#"pl.col('x').mean()"#, "expression node"),
    ] {
        let error = host
            .compile(source, ExpressionKind::Enrichment, &AtomicBool::new(false))
            .unwrap_err();
        assert!(error.to_string().contains(expected), "{source}: {error}");
    }
}

#[test]
fn compile_boundary_rejects_nonlocal_functions_for_every_purpose_without_rows() {
    for kind in [
        ExpressionKind::Enrichment,
        ExpressionKind::Filter,
        ExpressionKind::Color,
    ] {
        let expression = if kind == ExpressionKind::Enrichment {
            col("x").shift(lit(1))
        } else {
            col("x").shift(lit(1)).gt(lit(0))
        };
        let json = serde_json::to_string(&expression).unwrap();
        let error = CompiledDefinition::compile("unsafe".into(), &json, kind).unwrap_err();
        assert!(error.to_string().contains("row-local"), "{kind:?}: {error}");
    }

    let empty = DataFrame::new(
        0,
        vec![Series::new_empty("x".into(), &DataType::Int64).into()],
    )
    .unwrap();
    assert!(validate_expression_for_frame(&empty, col("x").shift(lit(1))).is_err());

    let hidden_shift = when(lit(false))
        .then(col("x").shift(lit(1)))
        .otherwise(col("x"));
    let hidden_json = serde_json::to_string(&hidden_shift).unwrap();
    assert!(
        CompiledDefinition::compile(
            "dead branch".into(),
            &hidden_json,
            ExpressionKind::Enrichment,
        )
        .is_err()
    );
}

#[test]
fn unsigned_json_provenance_survives_float_projection() {
    let source = SourceId::new();
    let raw = record(
        source,
        0,
        br#"{"huge":18446744073709551615}"#,
        ChunkPosition::Complete,
    );
    let bytes = raw.bytes.clone();
    let batch = records_to_batch(std::slice::from_ref(&raw)).unwrap();
    assert_eq!(
        batch.frame.column("huge").unwrap().dtype(),
        &DataType::Float64
    );
    assert_eq!(
        batch
            .frame
            .column("_lvu_type_huge")
            .unwrap()
            .str()
            .unwrap()
            .get(0),
        Some("uint64")
    );
    assert_eq!(raw.bytes, bytes);
}

fn exact_matches(
    records: &[RawRecord],
    context: &mut SchemaContext,
    constraint: &ExactFieldConstraint,
    filter: Option<&CompiledDefinition>,
) -> Vec<u64> {
    let batch =
        records_to_batch_with_context_and_exact_field(records, context, Some(constraint.field()))
            .unwrap();
    execute_batch_with_exact_constraint(
        &batch.frame,
        BatchQuery {
            generation: 1,
            definition_generation: 1,
            stages: &[],
            filter,
            text_search: None,
            colors: &[],
            column_colors: &[],
        },
        Some(constraint),
    )
    .matched_ids
    .into_iter()
    .map(|id| id.sequence)
    .collect()
}

#[test]
fn exact_correlation_uses_full_typed_values_and_preserves_raw() {
    let source = SourceId::new();
    let prefix = "x".repeat(512);
    let first = format!(r#"{{"key":"{prefix}a"}}"#);
    let second = format!(r#"{{"key":"{prefix}b"}}"#);
    let records = [
        record(source, 1, first.as_bytes(), ChunkPosition::Complete),
        record(source, 2, second.as_bytes(), ChunkPosition::Complete),
        record(source, 3, br#"{"key":1}"#, ChunkPosition::Complete),
        record(source, 4, br#"{"key":1.0}"#, ChunkPosition::Complete),
        record(source, 5, br#"{"key":"1"}"#, ChunkPosition::Complete),
        record(source, 6, br#"{"key":true}"#, ChunkPosition::Complete),
    ];
    let selected = resolve_exact_field(&records[0], "key").unwrap();
    let constraint = ExactFieldConstraint::new("key", selected).unwrap();
    assert_eq!(
        exact_matches(&records, &mut SchemaContext::default(), &constraint, None),
        [1]
    );
    assert_eq!(records[0].bytes, first.as_bytes());

    for (sequence, scalar) in [
        (3, ExactScalar::SignedInteger(1)),
        (4, ExactScalar::finite_float(1.0).unwrap()),
        (5, ExactScalar::string("1").unwrap()),
        (6, ExactScalar::Bool(true)),
    ] {
        let constraint = ExactFieldConstraint::new("key", scalar).unwrap();
        assert_eq!(
            exact_matches(&records, &mut SchemaContext::default(), &constraint, None),
            [sequence]
        );
    }
}

#[test]
fn exact_correlation_distinguishes_null_missing_conflicts_and_large_unsigned() {
    let source = SourceId::new();
    let records = [
        record(source, 1, br#"{"key":null}"#, ChunkPosition::Complete),
        record(source, 2, br#"{"other":null}"#, ChunkPosition::Complete),
        record(source, 3, br#"{"key":"true"}"#, ChunkPosition::Complete),
        record(source, 4, br#"{"key":true}"#, ChunkPosition::Complete),
        record(
            source,
            5,
            br#"{"key":18446744073709551615}"#,
            ChunkPosition::Complete,
        ),
        record(
            source,
            6,
            br#"{"key":18446744073709551614}"#,
            ChunkPosition::Complete,
        ),
    ];
    for (scalar, expected) in [
        (ExactScalar::Null, vec![1]),
        (ExactScalar::string("true").unwrap(), vec![3]),
        (ExactScalar::Bool(true), vec![4]),
        (ExactScalar::UnsignedInteger(u64::MAX), vec![5]),
    ] {
        let constraint = ExactFieldConstraint::new("key", scalar).unwrap();
        assert_eq!(
            exact_matches(&records, &mut SchemaContext::default(), &constraint, None),
            expected
        );
    }
    assert_eq!(
        resolve_exact_field(&records[1], "key"),
        Err(ExactValueError::Missing)
    );
}

#[test]
fn exact_correlation_is_partition_independent_and_ands_existing_filter() {
    let source = SourceId::new();
    let records = (1..=6)
        .map(|sequence| {
            let bytes = format!(r#"{{"key":"same","keep":{}}}"#, sequence % 2 == 0);
            record(source, sequence, bytes.as_bytes(), ChunkPosition::Complete)
        })
        .collect::<Vec<_>>();
    let constraint =
        ExactFieldConstraint::new("key", ExactScalar::string("same").unwrap()).unwrap();
    let filter = definition("keep", col("keep"), ExpressionKind::Filter);
    let whole = exact_matches(
        &records,
        &mut SchemaContext::default(),
        &constraint,
        Some(&filter),
    );
    let mut context = SchemaContext::default();
    let mut partitioned = Vec::new();
    for part in records.chunks(2) {
        partitioned.extend(exact_matches(
            part,
            &mut context,
            &constraint,
            Some(&filter),
        ));
    }
    assert_eq!(whole, vec![2, 4, 6]);
    assert_eq!(partitioned, whole);
}

#[test]
fn exact_correlation_handles_escaped_control_and_unicode_strings() {
    let source = SourceId::new();
    let records = [
        record(
            source,
            1,
            r#"{"key":"quote\" line\n snowman ☃"}"#.as_bytes(),
            ChunkPosition::Complete,
        ),
        record(
            source,
            2,
            r#"{"key":"quote\" line\t snowman ☃"}"#.as_bytes(),
            ChunkPosition::Complete,
        ),
    ];
    let scalar = resolve_exact_field(&records[0], "key").unwrap();
    let constraint = ExactFieldConstraint::new("key", scalar).unwrap();
    assert_eq!(
        exact_matches(&records, &mut SchemaContext::default(), &constraint, None),
        [1]
    );
}

#[test]
fn exact_correlation_refuses_bounds_reserved_complex_and_nonfinite_values() {
    assert!(ExactFieldConstraint::new("x".repeat(65), ExactScalar::Null).is_err());
    assert!(ExactFieldConstraint::new("raw", ExactScalar::Null).is_err());
    assert!(ExactFieldConstraint::new("_lvu_raw", ExactScalar::Null).is_err());
    assert!(ExactScalar::string("x".repeat(MAX_EXACT_SCALAR_BYTES + 1)).is_err());
    assert!(ExactScalar::finite_float(f64::NAN).is_err());
    let oversized = format!(
        r#"{{"field":"key","value":{{"kind":"string","value":"{}"}}}}"#,
        "x".repeat(MAX_EXACT_SCALAR_BYTES + 1)
    );
    assert!(serde_json::from_str::<ExactFieldConstraint>(&oversized).is_err());
    let nonfinite = format!(
        r#"{{"kind":"float_bits","value":{}}}"#,
        f64::INFINITY.to_bits()
    );
    assert!(serde_json::from_str::<ExactScalar>(&nonfinite).is_err());

    let source = SourceId::new();
    let object = record(
        source,
        1,
        br#"{"key":{"nested":1}}"#,
        ChunkPosition::Complete,
    );
    let array = record(source, 2, br#"{"key":[1,2]}"#, ChunkPosition::Complete);
    assert_eq!(
        resolve_exact_field(&object, "key"),
        Err(ExactValueError::UnsupportedType)
    );
    assert_eq!(
        resolve_exact_field(&array, "key"),
        Err(ExactValueError::UnsupportedType)
    );
}

#[test]
fn exact_projection_explicitly_refuses_requested_field_beyond_schema_cap() {
    let source = SourceId::new();
    let mut fields = (0..256)
        .map(|index| format!(r#""a{index:03}":{index}"#))
        .collect::<Vec<_>>();
    fields.push(r#""zzz_target":"full-value""#.into());
    let json = format!("{{{}}}", fields.join(","));
    let mut logfmt_fields = (0..256)
        .map(|index| format!("a{index:03}={index}"))
        .collect::<Vec<_>>();
    logfmt_fields.push("zzz_target=full-value".into());
    let logfmt = logfmt_fields.join(" ");
    let records = [
        record(source, 1, json.as_bytes(), ChunkPosition::Complete),
        record(source, 2, logfmt.as_bytes(), ChunkPosition::Complete),
    ];
    assert_eq!(
        resolve_exact_field(&records[0], "zzz_target"),
        Err(ExactValueError::ProjectionUnavailable)
    );
    assert_eq!(
        resolve_exact_field(&records[1], "zzz_target"),
        Err(ExactValueError::ProjectionUnavailable)
    );
    assert!(
        records_to_batch_with_context_and_exact_field(
            &records,
            &mut SchemaContext::default(),
            Some("zzz_target")
        )
        .is_err()
    );

    let admitted = format!("{{{}}}", fields[..256].join(","));
    let ordinary = record(source, 3, admitted.as_bytes(), ChunkPosition::Complete);
    let target = record(
        source,
        4,
        br#"{"zzz_target":"full-value"}"#,
        ChunkPosition::Complete,
    );
    let mut full_context = SchemaContext::default();
    records_to_batch_with_context(std::slice::from_ref(&ordinary), &mut full_context).unwrap();
    assert!(
        records_to_batch_with_context_and_exact_field(
            std::slice::from_ref(&target),
            &mut full_context,
            Some("zzz_target")
        )
        .is_err()
    );
}

#[test]
fn exact_execution_rejects_projection_for_a_different_field() {
    let source = SourceId::new();
    let records = [record(
        source,
        1,
        br#"{"field_a":"same","field_b":"same"}"#,
        ChunkPosition::Complete,
    )];
    let mut context = SchemaContext::default();
    let batch =
        records_to_batch_with_context_and_exact_field(&records, &mut context, Some("field_a"))
            .unwrap();
    let constraint =
        ExactFieldConstraint::new("field_b", ExactScalar::string("same").unwrap()).unwrap();
    let result = execute_batch_with_exact_constraint(
        &batch.frame,
        BatchQuery {
            generation: 1,
            definition_generation: 1,
            stages: &[],
            filter: None,
            text_search: None,
            colors: &[],
            column_colors: &[],
        },
        Some(&constraint),
    );
    assert_eq!(result.validity, BatchValidity::InvalidFilter);
    assert!(result.matched_ids.is_empty());
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "exact_projection_unavailable")
    );
}

#[test]
fn exact_correlation_refuses_lossy_utf8_instead_of_matching_replacement_text() {
    let source = SourceId::new();
    let invalid = record(source, 1, b"{\"key\":\"\xff\"}", ChunkPosition::Complete);
    let replacement = record(
        source,
        2,
        r#"{"key":"�"}"#.as_bytes(),
        ChunkPosition::Complete,
    );
    assert_eq!(
        resolve_exact_field(&invalid, "key"),
        Err(ExactValueError::InvalidUtf8)
    );
    let ordinary = records_to_batch(std::slice::from_ref(&invalid)).unwrap();
    assert_eq!(
        ordinary.frame.column("key").unwrap().str().unwrap().get(0),
        Some("�")
    );
    let constraint = ExactFieldConstraint::new("key", ExactScalar::string("�").unwrap()).unwrap();
    assert_eq!(
        exact_matches(
            &[invalid, replacement],
            &mut SchemaContext::default(),
            &constraint,
            None,
        ),
        [2]
    );
}

#[test]
fn repeated_logfmt_key_preserves_ordinary_parse_and_exact_last_value() {
    let source = SourceId::new();
    let mut tokens = (0..300)
        .map(|index| format!("key=value-{index}"))
        .collect::<Vec<_>>();
    tokens.push("other=visible".into());
    let text = tokens.join(" ");
    let selected = record(source, 1, text.as_bytes(), ChunkPosition::Complete);

    let ordinary = records_to_batch(std::slice::from_ref(&selected)).unwrap();
    assert_eq!(ordinary.parse_status, [ParseStatus::Logfmt]);
    assert_eq!(
        ordinary.frame.column("key").unwrap().str().unwrap().get(0),
        Some("value-299")
    );
    assert_eq!(
        ordinary
            .frame
            .column("other")
            .unwrap()
            .str()
            .unwrap()
            .get(0),
        Some("visible")
    );

    let scalar = resolve_exact_field(&selected, "key").unwrap();
    assert_eq!(scalar, ExactScalar::string("value-299").unwrap());
    let constraint = ExactFieldConstraint::new("key", scalar).unwrap();
    assert_eq!(
        exact_matches(
            &[selected],
            &mut SchemaContext::default(),
            &constraint,
            None,
        ),
        [1]
    );
}

/// W19 diagnosis for W13's `test_lvu_real_pty` failure: the message a user saw
/// was `expression cannot be lowered for this schema: unable to find column
/// "error_flag"`, which is what the engine said when a filter named a column
/// absent from the current typed batch.
///
/// That is a different case from `failed_overwrite_fences_ast_dependents_but_not_literals`
/// above, where the stage ran and failed: `failed_fields` catches that one and
/// answers `filter dependency … failed in this generation`. When the column is
/// absent from this batch, the engine now answers per batch
/// (`filter needs "error_flag", which is not available in this batch`,
/// `unknown_field`) instead of Polars' lowering implementation. Absence here
/// does not prove no stage produces the column in another batch, so the message
/// never claims a global schema. Validity stays `InvalidFilter`, so the caller
/// keeps the last-good complete chain. No race is asserted: absent could be a
/// typo or a stale revision, and the diagnostic does not distinguish them
/// without revision evidence the engine does not hold.
#[test]
fn a_filter_naming_a_column_no_stage_produces_reports_actionable_diagnostic() {
    let source = SourceId::new();
    let input = records_to_batch(&[record(source, 1, b"level=ERROR", ChunkPosition::Complete)])
        .unwrap()
        .frame;
    // The chain the recipe meant to carry produces `error_flag`; this is the
    // same query with that stage missing.
    let filter = definition(
        "pl.col('error_flag')",
        col("error_flag"),
        ExpressionKind::Filter,
    );
    let output = execute_batch(
        &input,
        BatchQuery {
            generation: 1,
            definition_generation: 1,
            stages: &[],
            filter: Some(&filter),
            text_search: None,
            colors: &[],
            column_colors: &[],
        },
    );
    assert_eq!(output.validity, BatchValidity::InvalidFilter);
    assert!(
        output.matched_ids.is_empty(),
        "an unresolvable filter matches nothing"
    );
    let diagnostic = output
        .diagnostics
        .iter()
        .find(|item| item.field.is_none())
        .expect("a filter diagnostic");
    assert_eq!(diagnostic.code, "unknown_field");
    assert!(
        diagnostic.message.contains("error_flag")
            && diagnostic.message.contains("not available in this batch"),
        "the diagnostic must name the cause per batch, not the implementation: {}",
        diagnostic.message
    );
    assert!(
        !diagnostic.message.contains("cannot be lowered")
            && !diagnostic.message.contains("unable to find column")
            && !diagnostic.message.contains("not produced by this chain"),
        "Polars lowering detail and global schema claims must not reach the user: {}",
        diagnostic.message
    );
    assert!(
        !output
            .diagnostics
            .iter()
            .any(|item| item.code == "dependency_unavailable"),
        "no stage failed, so the dependency guard cannot be what answers: {:?}",
        output.diagnostics
    );
}

#[test]
fn absent_output_is_distinct_from_failed_stage() {
    let source = SourceId::new();
    let input = records_to_batch(&[record(source, 1, b"x=old", ChunkPosition::Complete)])
        .unwrap()
        .frame;
    // `x` runs and fails (its own input is absent from this batch), so a filter
    // on `x` is a failed dependency. `never_produced` names a column absent
    // from this batch and from the stage list. Real expressions in both cases;
    // the codes must differ.
    let failing = [EnrichmentStage {
        name: "x".into(),
        definition: definition(
            "pl.col('absent_base')",
            col("absent_base"),
            ExpressionKind::Enrichment,
        ),
    }];
    let failed_filter = definition(
        "pl.col('x') == 'old'",
        col("x").eq(lit("old")),
        ExpressionKind::Filter,
    );
    let failed = execute_batch(
        &input,
        BatchQuery {
            generation: 1,
            definition_generation: 1,
            stages: &failing,
            filter: Some(&failed_filter),
            text_search: None,
            colors: &[],
            column_colors: &[],
        },
    );
    assert_eq!(failed.validity, BatchValidity::InvalidFilter);
    assert!(
        failed
            .diagnostics
            .iter()
            .any(|item| item.field.is_none() && item.code == "dependency_unavailable"),
        "failed stage must stay dependency_unavailable: {:?}",
        failed.diagnostics
    );

    let absent_filter = definition(
        "pl.col('never_produced')",
        col("never_produced"),
        ExpressionKind::Filter,
    );
    let absent = execute_batch(
        &input,
        BatchQuery {
            generation: 1,
            definition_generation: 1,
            stages: &[],
            filter: Some(&absent_filter),
            text_search: None,
            colors: &[],
            column_colors: &[],
        },
    );
    assert_eq!(absent.validity, BatchValidity::InvalidFilter);
    let diagnostic = absent
        .diagnostics
        .iter()
        .find(|item| item.field.is_none())
        .expect("an absent-output diagnostic");
    assert_eq!(diagnostic.code, "unknown_field");
    assert!(
        diagnostic.message.contains("never_produced")
            && diagnostic.message.contains("not available in this batch"),
        "{}",
        diagnostic.message
    );
}

#[test]
fn valid_null_column_is_not_an_absent_output() {
    let source = SourceId::new();
    // A `Null` column exists in the typed batch, so a predicate over it is
    // valid (matching nothing), not an unknown field. This distinguishes valid
    // null from absent output without scanning user data.
    let null_only = records_to_batch(&[record(
        source,
        1,
        br#"{"status":null}"#,
        ChunkPosition::Complete,
    )])
    .unwrap()
    .frame;
    assert_eq!(null_only.column("status").unwrap().dtype(), &DataType::Null);
    let filter = definition(
        "pl.col('status') == 500",
        col("status").eq(lit(500_i64)),
        ExpressionKind::Filter,
    );
    let valid = execute_batch(
        &null_only,
        BatchQuery {
            generation: 1,
            definition_generation: 1,
            stages: &[],
            filter: Some(&filter),
            text_search: None,
            colors: &[],
            column_colors: &[],
        },
    );
    assert_eq!(
        valid.validity,
        BatchValidity::Valid,
        "{:?}",
        valid.diagnostics
    );
    assert!(valid.matched_ids.is_empty());
    assert!(
        !valid
            .diagnostics
            .iter()
            .any(|item| item.code == "unknown_field"),
        "{:?}",
        valid.diagnostics
    );

    // The same filter against a batch whose typed projection has no `status`
    // column at all is absent, not valid null.
    let unstructured =
        records_to_batch(&[record(source, 2, b"plain text", ChunkPosition::Complete)])
            .unwrap()
            .frame;
    assert!(unstructured.column("status").is_err());
    let absent = execute_batch(
        &unstructured,
        BatchQuery {
            generation: 1,
            definition_generation: 1,
            stages: &[],
            filter: Some(&filter),
            text_search: None,
            colors: &[],
            column_colors: &[],
        },
    );
    assert_eq!(absent.validity, BatchValidity::InvalidFilter);
    assert!(
        absent
            .diagnostics
            .iter()
            .any(|item| item.field.is_none() && item.code == "unknown_field"),
        "{:?}",
        absent.diagnostics
    );
}

#[test]
fn enrichment_stage_naming_absent_column_reports_actionable_diagnostic() {
    let source = SourceId::new();
    let input = records_to_batch(&[record(source, 1, b"x=1", ChunkPosition::Complete)])
        .unwrap()
        .frame;
    let stages = [EnrichmentStage {
        name: "broken".into(),
        definition: definition(
            "pl.col('absent_base')",
            col("absent_base"),
            ExpressionKind::Enrichment,
        ),
    }];
    let output = execute_batch(
        &input,
        BatchQuery {
            generation: 1,
            definition_generation: 1,
            stages: &stages,
            filter: None,
            text_search: None,
            colors: &[],
            column_colors: &[],
        },
    );
    assert!(output.enriched_rows.column("broken").is_err());
    let diagnostic = output
        .diagnostics
        .iter()
        .find(|item| item.field.as_deref() == Some("broken"))
        .expect("a stage diagnostic");
    assert_eq!(diagnostic.code, "unknown_field");
    assert!(
        diagnostic.message.contains("absent_base")
            && diagnostic.message.contains("not available in this batch"),
        "{}",
        diagnostic.message
    );
    assert!(
        !diagnostic.message.contains("cannot be lowered"),
        "{}",
        diagnostic.message
    );
}

#[test]
fn stage_depending_on_later_stage_names_the_ordering_per_batch() {
    let source = SourceId::new();
    let input = records_to_batch(&[record(source, 1, b"x=1", ChunkPosition::Complete)])
        .unwrap()
        .frame;
    // `early` reads `later`, which is declared downstream. At `early`'s turn
    // `later` has not produced anything yet in this batch. Real expressions;
    // the diagnostic must name the ordering, not a global absence.
    let stages = [
        EnrichmentStage {
            name: "early".into(),
            definition: definition("pl.col('later')", col("later"), ExpressionKind::Enrichment),
        },
        EnrichmentStage {
            name: "later".into(),
            definition: definition("pl.lit('ok')", lit("ok"), ExpressionKind::Enrichment),
        },
    ];
    let output = execute_batch(
        &input,
        BatchQuery {
            generation: 1,
            definition_generation: 1,
            stages: &stages,
            filter: None,
            text_search: None,
            colors: &[],
            column_colors: &[],
        },
    );
    assert!(output.enriched_rows.column("early").is_err());
    assert!(
        output
            .enriched_rows
            .column("later")
            .unwrap()
            .str()
            .unwrap()
            .get(0)
            == Some("ok")
    );
    let diagnostic = output
        .diagnostics
        .iter()
        .find(|item| item.field.as_deref() == Some("early"))
        .expect("an ordering diagnostic");
    assert_eq!(diagnostic.code, "unknown_field");
    assert!(
        diagnostic.message.contains("later") && diagnostic.message.contains("later stage"),
        "must name the forward reference, not a global absence: {}",
        diagnostic.message
    );
}

#[test]
fn heterogeneous_batches_keep_shared_schema_nulls_but_fresh_batches_stay_per_batch() {
    let source = SourceId::new();
    // With a shared `SchemaContext` (the view's historical behaviour), a field
    // seen in one batch stays known: a later batch without it still projects a
    // null column, so a predicate over it is valid (matching nothing), not an
    // unknown field.
    let mut shared = SchemaContext::default();
    let first = records_to_batch_with_context(
        &[record(
            source,
            1,
            br#"{"status":500}"#,
            ChunkPosition::Complete,
        )],
        &mut shared,
    )
    .unwrap()
    .frame;
    let second = records_to_batch_with_context(
        &[record(source, 2, b"plain text", ChunkPosition::Complete)],
        &mut shared,
    )
    .unwrap()
    .frame;
    assert!(first.column("status").is_ok());
    assert!(
        second.column("status").is_ok(),
        "shared schema must project historical nulls, not drop the column"
    );
    let filter = definition(
        "pl.col('status') == 500",
        col("status").eq(lit(500_i64)),
        ExpressionKind::Filter,
    );
    for frame in [&first, &second] {
        let output = execute_batch(
            frame,
            BatchQuery {
                generation: 1,
                definition_generation: 1,
                stages: &[],
                filter: Some(&filter),
                text_search: None,
                colors: &[],
                column_colors: &[],
            },
        );
        assert_eq!(
            output.validity,
            BatchValidity::Valid,
            "shared-schema nulls stay valid: {:?}",
            output.diagnostics
        );
        assert!(
            !output
                .diagnostics
                .iter()
                .any(|item| item.code == "unknown_field"),
            "{:?}",
            output.diagnostics
        );
    }
    assert_eq!(
        execute_batch(
            &first,
            BatchQuery {
                generation: 1,
                definition_generation: 1,
                stages: &[],
                filter: Some(&filter),
                text_search: None,
                colors: &[],
                column_colors: &[],
            },
        )
        .matched_ids
        .len(),
        1
    );
    assert!(
        execute_batch(
            &second,
            BatchQuery {
                generation: 1,
                definition_generation: 1,
                stages: &[],
                filter: Some(&filter),
                text_search: None,
                colors: &[],
                column_colors: &[],
            },
        )
        .matched_ids
        .is_empty()
    );

    // With fresh contexts (independent batches), the second batch truly has no
    // `status` column. Absence is per batch: the same filter is valid on the
    // first batch and `unknown_field` on the second, without claiming the field
    // is absent everywhere.
    let fresh_first = records_to_batch(&[record(
        source,
        1,
        br#"{"status":500}"#,
        ChunkPosition::Complete,
    )])
    .unwrap()
    .frame;
    let fresh_second =
        records_to_batch(&[record(source, 2, b"plain text", ChunkPosition::Complete)])
            .unwrap()
            .frame;
    assert!(fresh_second.column("status").is_err());
    assert_eq!(
        execute_batch(
            &fresh_first,
            BatchQuery {
                generation: 1,
                definition_generation: 1,
                stages: &[],
                filter: Some(&filter),
                text_search: None,
                colors: &[],
                column_colors: &[],
            },
        )
        .validity,
        BatchValidity::Valid
    );
    let absent = execute_batch(
        &fresh_second,
        BatchQuery {
            generation: 1,
            definition_generation: 1,
            stages: &[],
            filter: Some(&filter),
            text_search: None,
            colors: &[],
            column_colors: &[],
        },
    );
    assert_eq!(absent.validity, BatchValidity::InvalidFilter);
    let diagnostic = absent
        .diagnostics
        .iter()
        .find(|item| item.field.is_none())
        .expect("a per-batch diagnostic");
    assert_eq!(diagnostic.code, "unknown_field");
    assert!(
        diagnostic.message.contains("not available in this batch"),
        "{}",
        diagnostic.message
    );
}

#[test]
fn absent_filter_aborts_candidate_and_preserves_published() {
    // Where this crate provides accepted-state preservation
    // (`execute_bounded_batches` + `BoundedPageSink`), an absent-output filter
    // must abort the candidate and leave the last-good publication intact.
    // App-level last-good rollback beyond this seam needs an app-boundary test;
    // see the completion report.
    let state = QueryGenerationState::new();
    let source = SourceId::new();
    let frame = records_to_batch(&[record(source, 1, b"x=yes", ChunkPosition::Complete)])
        .unwrap()
        .frame;
    let mut sink = BoundedPageSink::new(4);
    let first = state.begin();
    assert!(execute_bounded_batches(
        &state,
        vec![frame.clone()],
        QueryExecution {
            generation: first,
            total_batches: Some(1),
            plan: QueryPlan {
                definition_generation: 1,
                stages: &[],
                filter: None,
                text_search: None,
                colors: &[],
                column_colors: &[]
            },
            cancellation: &QueryCancellation::new(),
        },
        &mut sink,
        |_| {}
    ));
    assert_eq!(sink.published().len(), 1);

    let missing = definition(
        "pl.col('error_flag')",
        col("error_flag"),
        ExpressionKind::Filter,
    );
    let second = state.begin();
    assert!(!execute_bounded_batches(
        &state,
        vec![frame],
        QueryExecution {
            generation: second,
            total_batches: Some(1),
            plan: QueryPlan {
                definition_generation: 2,
                stages: &[],
                filter: Some(&missing),
                text_search: None,
                colors: &[],
                column_colors: &[]
            },
            cancellation: &QueryCancellation::new()
        },
        &mut sink,
        |_| {},
    ));
    assert_eq!(state.committed_generation(), Some(first));
    assert_eq!(sink.published().len(), 1);
}

/// Column classification rules evaluate natively over the typed enriched
/// frame and only for compiled stage outputs: full values decide, so strings
/// sharing a display prefix still discriminate, numbers and booleans compare
/// by canonical text form, nulls never match, empty wants match only empty
/// cells, and failed, missing or never-produced columns silently match
/// nothing without failing the batch. A raw same-name column present in the
/// frame without a producing stage paints nothing.
#[test]
fn column_colors_match_typed_cells_exactly() {
    let prefix = "p".repeat(600);
    let long_a = format!("{prefix}A");
    let long_b = format!("{prefix}B");
    let long_c = format!("{prefix}C");
    let frame = df!(
        "_lvu_source_id" => ["s", "s", "s"],
        "_lvu_sequence" => [0u64, 1u64, 2u64],
        "big" => [long_a.clone(), long_b.clone(), long_c.clone()],
        "num" => [42i64, 7i64, 0i64],
        "float" => [42.5f64, 1.0f64, -0.0f64],
        "flag" => [true, false, false],
        "nothing" => [Option::<String>::None, None, None],
        "empty" => ["", "x", "y"],
        // Raw projection namesake with no producing stage below.
        "rawname" => ["a", "b", "c"],
    )
    .unwrap();
    // Identity stages declare the compiled outputs under test; a
    // protected-name stage fails structurally and contributes no column.
    let identity = |name: &str| EnrichmentStage {
        name: name.into(),
        definition: definition(
            &format!("col {name}"),
            col(name),
            ExpressionKind::Enrichment,
        ),
    };
    let stages = vec![
        identity("big"),
        identity("num"),
        identity("float"),
        identity("flag"),
        identity("nothing"),
        identity("empty"),
        EnrichmentStage {
            name: "_lvu_bad".into(),
            definition: definition("pl.lit(1)", lit(1), ExpressionKind::Enrichment),
        },
    ];
    let rules = vec![
        ("long-a".to_string(), "big".to_string(), long_a.clone()),
        ("long-b".to_string(), "big".to_string(), long_b.clone()),
        ("long-c".to_string(), "big".to_string(), long_c.clone()),
        // The shared 512-byte prefix alone matches neither row exactly.
        ("prefix".to_string(), "big".to_string(), prefix.clone()),
        ("int".to_string(), "num".to_string(), "42".to_string()),
        (
            "int-padded".to_string(),
            "num".to_string(),
            "042".to_string(),
        ),
        ("float".to_string(), "float".to_string(), "42.5".to_string()),
        (
            "float-truncated".to_string(),
            "float".to_string(),
            "42".to_string(),
        ),
        // Presented float values match exactly as displayed.
        (
            "float-one".to_string(),
            "float".to_string(),
            "1.0".to_string(),
        ),
        (
            "float-negzero".to_string(),
            "float".to_string(),
            "-0.0".to_string(),
        ),
        (
            "float-zero".to_string(),
            "float".to_string(),
            "0.0".to_string(),
        ),
        ("bool".to_string(), "flag".to_string(), "true".to_string()),
        (
            "null-text".to_string(),
            "nothing".to_string(),
            "null".to_string(),
        ),
        (
            "null-empty".to_string(),
            "nothing".to_string(),
            String::new(),
        ),
        ("empty".to_string(), "empty".to_string(), String::new()),
        (
            "empty-miss".to_string(),
            "severity".to_string(),
            String::new(),
        ),
        (
            "missing".to_string(),
            "missing".to_string(),
            "x".to_string(),
        ),
        (
            "raw-namesake".to_string(),
            "rawname".to_string(),
            "a".to_string(),
        ),
        (
            "failed".to_string(),
            "_lvu_bad".to_string(),
            "1".to_string(),
        ),
    ];
    let result = execute_batch(
        &frame,
        BatchQuery {
            generation: 1,
            definition_generation: 1,
            stages: &stages,
            filter: None,
            text_search: None,
            colors: &[],
            column_colors: &rules,
        },
    );
    assert_eq!(result.validity, BatchValidity::Valid);
    assert!(result.color_diagnostics.is_empty());
    let sequences = |name: &str| {
        result
            .color_matches
            .get(name)
            .map(|ids| ids.iter().map(|id| id.sequence).collect::<Vec<_>>())
            .unwrap_or_default()
    };
    // Full 601-byte values discriminate where a 512-byte display projection
    // could not: each names exactly its own row.
    assert_eq!(sequences("long-a"), vec![0]);
    assert_eq!(sequences("long-b"), vec![1]);
    assert_eq!(sequences("long-c"), vec![2]);
    assert!(sequences("prefix").is_empty());
    // Typed cells compare by canonical text form.
    assert_eq!(sequences("int"), vec![0]);
    assert!(sequences("int-padded").is_empty());
    assert_eq!(sequences("float"), vec![0]);
    assert!(sequences("float-truncated").is_empty());
    assert_eq!(sequences("float-one"), vec![1]);
    assert_eq!(sequences("float-negzero"), vec![2]);
    assert!(sequences("float-zero").is_empty());
    assert_eq!(sequences("bool"), vec![0]);
    // Null is never equal to anything, not even its display text.
    assert!(sequences("null-text").is_empty());
    assert!(sequences("null-empty").is_empty());
    // An empty want matches only literal empty-string ready cells.
    assert_eq!(sequences("empty"), vec![0]);
    assert!(sequences("empty-miss").is_empty());
    // Failed, missing and never-produced columns silently match nothing,
    // never an error — including a raw frame column with no stage behind it.
    assert!(sequences("missing").is_empty());
    assert!(sequences("raw-namesake").is_empty());
    assert!(sequences("failed").is_empty());
}
