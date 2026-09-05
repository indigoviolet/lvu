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
                colors: &[]
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
                colors: &[]
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
                colors: &[]
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
                colors: &[]
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
                colors: &[]
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
        },
    );
    assert_eq!(bad.validity, BatchValidity::InvalidFilter);
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
        definition(
            "pl.col('x') == 'old'",
            col("x").eq(lit("old")),
            ExpressionKind::Color,
        ),
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
        },
    );
    assert!(!output.color_matches.contains_key("stale"));
    assert!(output.diagnostics.iter().any(
        |item| item.field.as_deref() == Some("stale") && item.code == "dependency_unavailable"
    ));
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
