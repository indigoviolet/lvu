//! Event-time recognition against messy, realistic source content.

use lvu_live::time::{
    EpochUnit, MAX_RECOGNITION_RECORD_BYTES, RecognitionOptions, TimeFieldRef, TimeFieldSelection,
    TimeFormat, TimeInterpretation, TimeOutcome, ZoneAssumption, recognize_record,
    recognize_sample,
};

/// 2026-09-05T12:30:45Z
const NOON: i64 = 1_788_611_445_000_000_000;

/// One source carrying every encoding a real deployment mixes together.
const MIXED: &[&str] = &[
    r#"{"timestamp":"2026-09-05T12:30:45.123456789Z","msg":"json rfc3339"}"#,
    r#"{"time":"2026-09-05T14:30:45+02:00","msg":"json offset"}"#,
    r#"{"time":"2026-09-05T14:30:45+0200","msg":"json basic offset"}"#,
    r#"time=2026-09-05T12:30:45Z level=info msg="logfmt utc""#,
    r#"{"ts":1788611445,"msg":"epoch seconds"}"#,
    r#"{"ts":"1788611445123","msg":"epoch millis as a string"}"#,
    r#"{"ts":1788611445123456,"msg":"epoch micros"}"#,
    r#"{"ts":1788611445123456789,"msg":"epoch nanos"}"#,
    "2026-09-05 12:30:45.123 INFO unstructured prefix",
    "Sep  5 12:30:45 host worker[7]: syslog line",
    r#"{"ts":"2026-09-05 12:30:45","msg":"no timezone"}"#,
    r#"{"ts":"nope","msg":"malformed value"}"#,
    r#"{"bytes":1788611445,"msg":"numeric but not a time"}"#,
    r#"{"duration_ms":1500,"msg":"small number"}"#,
    "plain line with no time at all",
];

fn read(raw: &str, options: &RecognitionOptions) -> TimeOutcome {
    recognize_record(raw.as_bytes(), options)
}

fn nanos(outcome: &TimeOutcome) -> Option<i64> {
    match outcome {
        TimeOutcome::Valid(reading) => Some(reading.unix_nanos),
        _ => None,
    }
}

fn diagnostic(outcome: &TimeOutcome) -> String {
    match outcome {
        TimeOutcome::Invalid { diagnostic, .. } => diagnostic.clone(),
        other => panic!("expected a diagnostic, got {other:?}"),
    }
}

#[test]
fn mixed_textual_and_epoch_encodings_resolve_to_the_same_instant() {
    let options = RecognitionOptions::default();
    let expected: [(usize, i64, TimeFormat); 8] = [
        (0, NOON + 123_456_789, TimeFormat::OffsetDateTime),
        (1, NOON, TimeFormat::OffsetDateTime),
        (2, NOON, TimeFormat::OffsetDateTime),
        (3, NOON, TimeFormat::OffsetDateTime),
        (4, NOON, TimeFormat::Epoch(EpochUnit::Seconds)),
        (
            5,
            NOON + 123_000_000,
            TimeFormat::Epoch(EpochUnit::Milliseconds),
        ),
        (
            6,
            NOON + 123_456_000,
            TimeFormat::Epoch(EpochUnit::Microseconds),
        ),
        (
            7,
            NOON + 123_456_789,
            TimeFormat::Epoch(EpochUnit::Nanoseconds),
        ),
    ];
    for (index, unix_nanos, format) in expected {
        match read(MIXED[index], &options) {
            TimeOutcome::Valid(reading) => {
                assert_eq!(reading.unix_nanos, unix_nanos, "record {index}");
                assert_eq!(reading.format, format, "record {index}");
                assert!(
                    reading.assumption.is_none(),
                    "record {index} needed no assumption"
                );
            }
            other => panic!("record {index} was not recognized: {other:?}"),
        }
    }
}

#[test]
fn a_missing_timezone_is_reported_rather_than_treated_as_utc() {
    let strict = RecognitionOptions::default();
    for raw in [MIXED[8], MIXED[10]] {
        let message = diagnostic(&read(raw, &strict));
        assert!(message.contains("no timezone"), "{raw}: {message}");
        assert!(message.contains("declare"), "{raw}: {message}");
    }

    let utc = RecognitionOptions {
        zone: ZoneAssumption::Utc,
        ..RecognitionOptions::default()
    };
    let reading = match read(MIXED[10], &utc) {
        TimeOutcome::Valid(reading) => reading,
        other => panic!("{other:?}"),
    };
    assert_eq!(reading.unix_nanos, NOON);
    assert_eq!(
        reading.assumption.as_deref(),
        Some("value has no timezone; assumed UTC"),
        "the assumption stays visible after it is applied"
    );

    // The same value is a different instant under a different declared zone,
    // which is exactly why it may not be assumed.
    let berlin = RecognitionOptions {
        zone: ZoneAssumption::FixedOffsetSeconds(2 * 3600),
        ..RecognitionOptions::default()
    };
    assert_eq!(
        nanos(&read(MIXED[10], &berlin)),
        Some(NOON - 2 * 3_600_000_000_000)
    );
    match read(MIXED[10], &berlin) {
        TimeOutcome::Valid(reading) => assert_eq!(
            reading.assumption.as_deref(),
            Some("value has no timezone; assumed UTC+02:00")
        ),
        other => panic!("{other:?}"),
    }
}

#[test]
fn syslog_prefixes_are_recognized_but_never_given_an_invented_year() {
    let utc = RecognitionOptions {
        zone: ZoneAssumption::Utc,
        ..RecognitionOptions::default()
    };
    let message = diagnostic(&read(MIXED[9], &utc));
    assert!(message.contains("no year"), "{message}");

    let dated = RecognitionOptions {
        assumed_year: Some(2026),
        ..utc.clone()
    };
    for raw in [
        MIXED[9],
        "<134>Sep  5 12:30:45 host worker: priority prefix",
        "Sep 05 12:30:45 host worker: zero padded day",
    ] {
        match read(raw, &dated) {
            TimeOutcome::Valid(reading) => {
                assert_eq!(reading.unix_nanos, NOON, "{raw}");
                assert_eq!(reading.format, TimeFormat::SyslogDateTime);
                let assumption = reading.assumption.expect("assumptions are reported");
                assert!(assumption.contains("assumed 2026"), "{assumption}");
                assert!(assumption.contains("assumed UTC"), "{assumption}");
            }
            other => panic!("{raw}: {other:?}"),
        }
    }
}

#[test]
fn numbers_that_are_not_times_are_left_alone() {
    let options = RecognitionOptions::default();
    // A plausible epoch under a key that does not name a time is not adopted,
    // and does not become a per-record diagnostic either.
    assert_eq!(read(MIXED[12], &options), TimeOutcome::Missing);
    let report = recognize_sample([MIXED[12].as_bytes()], &options);
    let candidate = report
        .candidates
        .iter()
        .find(|candidate| candidate.label == "bytes")
        .expect("the field is still offered, with the reason it was not used");
    let blocked = candidate.blocked.clone().expect("blocked reason");
    assert!(blocked.contains("not a recognised time key"), "{blocked}");
    assert_eq!(report.selected, None);
    // A number outside the plausible window is not a time in any unit.
    assert_eq!(read(MIXED[13], &options), TimeOutcome::Missing);
    assert_eq!(read(MIXED[14], &options), TimeOutcome::Missing);
    assert!(diagnostic(&read(MIXED[11], &options)).contains("not a date-time"));

    let implausible = diagnostic(&read(r#"{"ts":42}"#, &options));
    assert!(
        implausible.contains("not a plausible epoch"),
        "{implausible}"
    );

    // A unit implied by the key never overrides what the values can be.
    let conflict = diagnostic(&read(r#"{"ts_ms":1788611445}"#, &options));
    assert!(
        conflict.contains("implies ms") && conflict.contains("plausible as s"),
        "{conflict}"
    );
    assert_eq!(
        nanos(&read(r#"{"ts_ms":1788611445123}"#, &options)),
        Some(NOON + 123_000_000)
    );
}

#[test]
fn no_epoch_value_is_plausible_in_two_units_at_once() {
    // The refusal path exists because the plausible window decides the unit;
    // this checks the window actually keeps the units apart, over the whole
    // decimal range each unit can express.
    let options = RecognitionOptions::default();
    for exponent in 0..19u32 {
        for lead in 1..10i128 {
            let value = lead * 10_i128.pow(exponent);
            let Ok(value) = i64::try_from(value) else {
                continue;
            };
            let outcome = read(&format!(r#"{{"ts":{value}}}"#), &options);
            if let TimeOutcome::Invalid { diagnostic, .. } = &outcome {
                assert!(
                    !diagnostic.contains("ambiguous"),
                    "{value} was ambiguous: {diagnostic}"
                );
            }
        }
    }
}

#[test]
fn a_time_named_key_that_cannot_be_read_never_silently_defers_to_another_field() {
    let options = RecognitionOptions::default();
    let outcome = read(
        r#"{"timestamp":"broken","started":"2026-09-05T12:30:45Z"}"#,
        &options,
    );
    match outcome {
        TimeOutcome::Invalid { field, diagnostic } => {
            assert_eq!(field, "timestamp");
            assert!(diagnostic.contains("not a date-time"), "{diagnostic}");
        }
        other => panic!("a broken time field must be reported, not skipped: {other:?}"),
    }

    // With no time-named key at all, any usable typed value is still found.
    match read(
        r#"{"started":"2026-09-05T12:30:45Z","level":"info"}"#,
        &options,
    ) {
        TimeOutcome::Valid(reading) => {
            assert_eq!(reading.field, "started");
            assert_eq!(reading.unix_nanos, NOON);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn timestamp_utc_is_an_output_name_and_holds_no_privileged_input_position() {
    let options = RecognitionOptions::default();
    match read(
        r#"{"timestamp_utc":"2026-09-05T12:30:45Z","ts":"2026-09-05T13:00:00Z"}"#,
        &options,
    ) {
        TimeOutcome::Valid(reading) => assert_eq!(
            reading.field, "ts",
            "the source's own key wins over lvu's output name"
        ),
        other => panic!("{other:?}"),
    }
}

#[test]
fn quoted_bodies_and_message_text_cannot_impersonate_the_record_time() {
    let options = RecognitionOptions {
        zone: ZoneAssumption::Utc,
        ..RecognitionOptions::default()
    };
    assert_eq!(
        read(
            r#"service=api message="failed at 2026-09-05 12:30:45 while retrying""#,
            &options
        ),
        TimeOutcome::Missing,
        "a timestamp inside a message body is not the record's event time"
    );
    // The same instant leading the record is a prefix, and is used.
    assert_eq!(
        nanos(&read("2026-09-05 12:30:45 service=api failed", &options)),
        Some(NOON)
    );
    assert_eq!(
        nanos(&read("[2026-09-05T12:30:45Z] service=api failed", &options)),
        Some(NOON)
    );
}

#[test]
fn malformed_wide_and_oversized_records_produce_diagnostics_never_panics() {
    let options = RecognitionOptions {
        zone: ZoneAssumption::Utc,
        ..RecognitionOptions::default()
    };
    let unterminated = diagnostic(&read(
        r#"message="unterminated ts=2026-09-05T12:30:45Z"#,
        &options,
    ));
    assert!(unterminated.contains("unterminated"), "{unterminated}");

    // Combining marks, wide glyphs and an emoji around the value.
    let wide = "{\"msg\":\"漢字 e\u{0301} 👩\u{200d}👩\u{200d}👧 ｗｉｄｅ\",\"ts\":\"2026-09-05T12:30:45Z\"}";
    assert_eq!(nanos(&read(wide, &options)), Some(NOON));
    assert_eq!(
        nanos(&read("2026-09-05T12:30:45Z 漢字 👩‍👩‍👧 tail", &options)),
        Some(NOON)
    );
    // A multi-byte character straddling the prefix window must not panic.
    let long = format!("{}{}", "漢".repeat(200), "2026-09-05T12:30:45Z");
    assert_eq!(read(&long, &options), TimeOutcome::Missing);

    // Invalid UTF-8 is scanned lossily and never rewritten.
    let mut invalid = vec![0xff, 0xfe, b' '];
    invalid.extend_from_slice(b"ts=2026-09-05T12:30:45Z");
    assert!(matches!(
        recognize_record(&invalid, &options),
        TimeOutcome::Valid(_)
    ));

    let oversized = vec![b'x'; MAX_RECOGNITION_RECORD_BYTES + 1];
    assert!(
        diagnostic(&recognize_record(&oversized, &options)).contains("bounded event-time"),
        "oversized records are refused, not truncated into a wrong answer"
    );
}

#[test]
fn ranking_prefers_the_field_that_actually_covers_the_source() {
    // `time` appears in one record; `started` in all of them. Coverage decides.
    let mut records: Vec<String> = (0..9)
        .map(|index| format!(r#"{{"started":"2026-09-05T12:30:4{index}Z","level":"info"}}"#))
        .collect();
    records.push(r#"{"time":"2026-09-05T12:30:45Z","started":"2026-09-05T12:30:45Z"}"#.into());
    let raws = records
        .iter()
        .map(|line| line.as_bytes())
        .collect::<Vec<_>>();
    let report = recognize_sample(raws.iter().copied(), &RecognitionOptions::default());
    assert_eq!(report.sampled_records, 10);
    assert_eq!(
        report.selected,
        Some(
            TimeFieldSelection::structured("started").with_interpretation(TimeInterpretation::Text)
        )
    );
    let summary = report
        .selected_summary()
        .expect("summary for the selection");
    assert_eq!(summary.parsed, 10);
    assert!((summary.coverage - 1.0).abs() < f64::EPSILON);
    let sparse = report
        .candidates
        .iter()
        .find(|candidate| candidate.label == "time")
        .expect("the sparse candidate stays visible");
    assert!((sparse.coverage - 0.1).abs() < 1e-9);
}

#[test]
fn a_field_read_as_different_epoch_units_is_refused_rather_than_averaged() {
    let records = [
        r#"{"ts":1788611445}"#.as_bytes(),
        r#"{"ts":1788611445123}"#.as_bytes(),
        r#"{"ts":1788611446}"#.as_bytes(),
    ];
    let report = recognize_sample(records.iter().copied(), &RecognitionOptions::default());
    assert_eq!(report.selected, None);
    let candidate = report
        .candidates
        .iter()
        .find(|candidate| candidate.label == "ts")
        .expect("ts candidate");
    let blocked = candidate.blocked.clone().expect("blocked reason");
    assert!(blocked.contains("s") && blocked.contains("ms"), "{blocked}");
    assert!(report.diagnostics.iter().any(|line| line.contains("ts")));

    // Declaring the unit resolves it, and the declaration is then authoritative.
    let declared = TimeFieldSelection::structured("ts")
        .with_interpretation(TimeInterpretation::Epoch(EpochUnit::Milliseconds));
    assert_eq!(nanos(&declared.read(records[1])), Some(NOON + 123_000_000));
    assert_eq!(
        nanos(&declared.read(records[0])),
        Some(1_788_611_445_000_000),
        "a declared unit is applied even where inference would have chosen seconds"
    );
}

#[test]
fn recognition_over_the_mixed_source_is_independent_of_batch_boundaries() {
    let options = RecognitionOptions {
        zone: ZoneAssumption::Utc,
        assumed_year: Some(2026),
        ..RecognitionOptions::default()
    };
    let raws = MIXED.iter().map(|line| line.as_bytes()).collect::<Vec<_>>();
    let whole = raws
        .iter()
        .map(|raw| recognize_record(raw, &options))
        .collect::<Vec<_>>();
    for partition in [1usize, 2, 4, 7, 100] {
        let partitioned = raws
            .chunks(partition)
            .flat_map(|batch| {
                batch
                    .iter()
                    .map(|raw| recognize_record(raw, &options))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        assert_eq!(partitioned, whole, "partition size {partition}");
    }

    // A selection proposed from one batch applies identically everywhere.
    let selection = recognize_sample(raws.iter().copied().take(4), &options)
        .selected
        .expect("a selection from the first batch");
    assert_eq!(selection.field, TimeFieldRef::Structured("time".into()));
    let applied = raws
        .iter()
        .map(|raw| nanos(&selection.read(raw)))
        .collect::<Vec<_>>();
    for partition in [1usize, 3, 5] {
        let repeated = raws
            .chunks(partition)
            .flat_map(|batch| batch.iter().map(|raw| nanos(&selection.read(raw))))
            .collect::<Vec<_>>();
        assert_eq!(repeated, applied, "partition size {partition}");
    }
    assert_eq!(applied.iter().filter(|value| value.is_some()).count(), 3);
}

#[test]
fn selections_survive_a_persistence_round_trip_and_reject_malformed_tokens() {
    let selections = [
        TimeFieldSelection::structured("meta.event.ts")
            .with_interpretation(TimeInterpretation::Epoch(EpochUnit::Microseconds))
            .with_zone(ZoneAssumption::FixedOffsetSeconds(-5 * 3600)),
        TimeFieldSelection::raw_prefix()
            .with_interpretation(TimeInterpretation::Text)
            .with_zone(ZoneAssumption::Utc)
            .with_assumed_year(Some(2026)),
        TimeFieldSelection::column("timestamp_utc"),
    ];
    for selection in selections {
        let token = selection.to_token();
        assert_eq!(TimeFieldSelection::parse_token(&token), Ok(selection));
    }
    assert!(TimeFieldSelection::parse_token("structured:ts|auto|utc").is_err());
    assert!(TimeFieldSelection::parse_token("mystery:ts|auto|utc|-").is_err());
    assert!(TimeFieldSelection::parse_token("structured:ts|epoch_weeks|utc|-").is_err());
}

#[test]
fn nested_structured_values_are_reachable_and_enriched_columns_are_not_guessed() {
    let options = RecognitionOptions::default();
    let nested = r#"{"meta":{"event":{"ts":"2026-09-05T12:30:45Z"}},"level":"info"}"#;
    match read(nested, &options) {
        TimeOutcome::Valid(reading) => assert_eq!(reading.field, "meta.event.ts"),
        other => panic!("{other:?}"),
    }
    let selection = TimeFieldSelection::structured("meta.event.ts");
    assert_eq!(nanos(&selection.read(nested.as_bytes())), Some(NOON));
    assert_eq!(
        selection.read(br#"{"level":"info"}"#),
        TimeOutcome::Missing,
        "a designated field that is absent yields nothing, never a fallback"
    );

    let column = TimeFieldSelection::column("timestamp_utc");
    assert!(
        diagnostic(&column.read(nested.as_bytes())).contains("query layer"),
        "raw bytes cannot resolve an enriched column"
    );
}
