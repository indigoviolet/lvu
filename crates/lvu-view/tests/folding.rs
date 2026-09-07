//! Observable behaviour of repeated-pattern folding: realistic floods, batch
//! independence, retention bounds, adversarial high-cardinality input, Unicode
//! and exact expansion fidelity.

use lvu::{DisplayRow, RowId};
use lvu_view::folding::{
    FoldConfig, FoldEngine, FoldEvent, FoldKey, FoldScope, Normalisation, character_count,
    expand_entries, fold_key, fold_rows, fold_rows_by, pattern_key, truncate_chars,
};

const SOURCE: &str = "0f2c1f10-0000-4000-8000-000000000001";

fn row(sequence: u64, level: &str, text: &str) -> DisplayRow {
    DisplayRow {
        id: RowId::new(SOURCE, sequence),
        timestamp: String::new(),
        captured_at_unix_nanos: Some(1_700_000_000_000_000_000 + sequence as i64 * 1_000_000),
        level: level.to_string(),
        text: text.to_string(),
        details: Vec::new(),
        fields: Vec::new(),
    }
}

fn plain(sequence: u64, text: &str) -> DisplayRow {
    row(sequence, "", text)
}

/// Distinct alphabetic token, so each event has a genuinely different shape
/// rather than one that normalisation would collapse.
fn word(mut value: u64) -> String {
    let mut out = String::new();
    loop {
        out.push((b'a' + (value % 26) as u8) as char);
        value /= 26;
        if value == 0 {
            break out;
        }
    }
}

fn ids(rows: &[DisplayRow]) -> Vec<RowId> {
    rows.iter().map(|row| row.id.clone()).collect()
}

fn folding() -> FoldConfig {
    FoldConfig::enabled()
}

/// A retry flood: same shape, different attempt numbers, latencies, endpoints
/// and timestamps. Distinct events on either side must stay separate.
#[test]
fn retry_flood_collapses_while_distinct_events_stay_visible() {
    let mut rows = vec![plain(0, "starting shipper for topic orders")];
    for attempt in 1..=40u64 {
        rows.push(plain(
            attempt,
            &format!(
                "2026-09-07T10:22:{:02}.884Z connection to 10.0.0.7:5432 failed after {}ms (attempt {})",
                attempt % 60,
                1500 + attempt * 7,
                attempt
            ),
        ));
    }
    rows.push(plain(41, "shipper flushed 12 pending batches"));

    let frame = fold_rows(folding(), &rows);

    assert_eq!(frame.entries.len(), 3, "{:?}", frame.entries);
    assert!(!frame.entries[0].folded);
    assert!(!frame.entries[2].folded);

    let flood = &frame.entries[1];
    assert!(flood.folded);
    assert_eq!(flood.count(), 40);
    assert_eq!(flood.first().id, RowId::new(SOURCE, 1));
    assert_eq!(flood.last().id, RowId::new(SOURCE, 40));
    assert_eq!(
        flood.first().timestamp_unix_nanos,
        rows[1].captured_at_unix_nanos
    );
    assert_eq!(
        flood.last().timestamp_unix_nanos,
        rows[40].captured_at_unix_nanos
    );
    assert_eq!(flood.first_sample, rows[1].text);
    assert_eq!(flood.last_sample, rows[40].text);
    assert!(
        flood.pattern.contains("<ip>") && flood.pattern.contains("<ts>"),
        "unexpected pattern {:?}",
        flood.pattern
    );

    // Reversible: nothing dropped, reordered or rewritten.
    assert_eq!(frame.expand(), ids(&rows));
}

/// A repeating stack trace: identical frames with volatile addresses, thread
/// names and object identifiers still share one shape.
#[test]
fn repeating_stack_trace_frames_share_one_pattern() {
    let mut rows = Vec::new();
    for repeat in 0..12u64 {
        rows.push(plain(
            repeat,
            &format!(
                "  at com.acme.Pool.lease(Pool.java:214) [worker-{}] handle=0x7ffe{:04x} session=\"{}\"",
                repeat + 3,
                repeat * 37,
                repeat
            ),
        ));
    }

    let frame = fold_rows(folding(), &rows);
    assert_eq!(frame.entries.len(), 1);
    assert_eq!(frame.entries[0].count(), 12);
    assert!(frame.entries[0].folded);
    assert!(frame.entries[0].pattern.contains("<hex>"));
    assert!(frame.entries[0].pattern.contains("<str>"));
    assert_eq!(frame.expand(), ids(&rows));
}

/// Different severities of the same shape are different patterns.
#[test]
fn severity_separates_otherwise_identical_shapes() {
    let rows = vec![
        row(0, "WARN", "queue depth 12"),
        row(1, "WARN", "queue depth 13"),
        row(2, "ERROR", "queue depth 14"),
        row(3, "ERROR", "queue depth 15"),
    ];
    let frame = fold_rows(folding(), &rows);
    assert_eq!(frame.entries.len(), 2);
    assert_eq!(frame.entries[0].count(), 2);
    assert_eq!(frame.entries[1].count(), 2);
    assert_ne!(frame.entries[0].pattern, frame.entries[1].pattern);
}

fn mixed_feed(count: u64) -> Vec<DisplayRow> {
    (0..count)
        .map(|sequence| match sequence % 7 {
            0 => plain(
                sequence,
                &format!(
                    "GET /api/v2/orders/{} -> 200 in {}ms",
                    sequence,
                    sequence % 40
                ),
            ),
            1..=3 => plain(
                sequence,
                &format!(
                    "timeout talking to 10.0.{}.4:9092, retrying in {}ms",
                    sequence % 5,
                    250 + sequence
                ),
            ),
            4 => row(
                sequence,
                "ERROR",
                &format!("checkpoint {} rejected: lag 0x{:x}", sequence, sequence),
            ),
            5 => plain(sequence, "heartbeat"),
            _ => plain(
                sequence,
                &format!("compacted segment /var/log/lvu/seg-{}.bin", sequence),
            ),
        })
        .collect()
}

fn fold_in_batches(config: FoldConfig, rows: &[DisplayRow], sizes: &[usize]) -> FoldEngine {
    let mut engine = FoldEngine::new(config);
    let mut offset = 0usize;
    let mut cursor = 0usize;
    while offset < rows.len() {
        let take = sizes[cursor % sizes.len()].max(1).min(rows.len() - offset);
        engine.extend(rows[offset..offset + take].iter().map(FoldEvent::from));
        offset += take;
        cursor += 1;
    }
    engine
}

/// Results must not depend on how the feed is chopped into arrivals.
#[test]
fn batch_boundaries_do_not_change_folding() {
    let rows = mixed_feed(400);
    for scope in [
        FoldScope::Adjacent,
        FoldScope::Lookback(4),
        FoldScope::Lookback(64),
    ] {
        let config = FoldConfig { scope, ..folding() };
        let whole = fold_rows(config, &rows);
        for sizes in [
            vec![1usize],
            vec![400],
            vec![3, 1, 17, 2, 55],
            vec![7, 7, 7],
            vec![199, 1],
        ] {
            let batched = fold_in_batches(config, &rows, &sizes).frame();
            assert_eq!(batched, whole, "scope {scope:?} sizes {sizes:?}");
        }
    }
}

/// The same equivalence must hold while retention eviction is firing, proving
/// eviction is a function of the prefix rather than of batch shape.
#[test]
fn batch_boundaries_do_not_change_folding_under_eviction() {
    let rows = mixed_feed(500);
    let config = FoldConfig {
        scope: FoldScope::Lookback(8),
        maximum_entries: 16,
        maximum_members: 64,
        maximum_patterns: 4,
        ..folding()
    };
    let whole = fold_rows(config, &rows);
    for sizes in [vec![1usize], vec![13, 2, 31], vec![500]] {
        assert_eq!(fold_in_batches(config, &rows, &sizes).frame(), whole);
    }
    assert!(whole.entries.len() <= 16);
    assert!(whole.total_members() <= 64);
    assert!(whole.stats.evicted_entries > 0);
}

/// Two interleaved floods only fold when a lookback window is configured;
/// adjacency alone must not merge across the interleave.
#[test]
fn lookback_window_folds_interleaved_floods() {
    let mut rows = Vec::new();
    for step in 0..20u64 {
        rows.push(plain(step * 2, &format!("disk queue depth {}", step)));
        rows.push(plain(step * 2 + 1, &format!("cache miss for key {}", step)));
    }

    let adjacent = fold_rows(folding(), &rows);
    assert_eq!(adjacent.entries.len(), 40);
    assert!(adjacent.entries.iter().all(|entry| !entry.folded));

    let windowed = fold_rows(
        FoldConfig {
            scope: FoldScope::Lookback(2),
            ..folding()
        },
        &rows,
    );
    assert_eq!(windowed.entries.len(), 2);
    assert_eq!(windowed.entries[0].count(), 20);
    assert_eq!(windowed.entries[1].count(), 20);
    assert!(windowed.entries.iter().all(|entry| entry.folded));

    // Interleaved membership still reconstructs the original order exactly.
    assert_eq!(expand_entries(&windowed.entries), ids(&rows));
    assert_eq!(windowed.entries[0].first().id, RowId::new(SOURCE, 0));
    assert_eq!(windowed.entries[1].first().id, RowId::new(SOURCE, 1));
}

/// Every input event stays individually addressable and appears exactly once.
#[test]
fn expansion_is_exact_and_every_event_stays_addressable() {
    let rows = mixed_feed(311);
    let config = FoldConfig {
        scope: FoldScope::Lookback(16),
        ..folding()
    };
    let frame = fold_rows(config, &rows);

    assert_eq!(frame.expand(), ids(&rows));
    assert_eq!(frame.total_members(), rows.len());
    assert!(frame.entries.len() < rows.len(), "nothing folded");

    for (index, row) in rows.iter().enumerate() {
        let entry = frame
            .entry_of(&row.id)
            .unwrap_or_else(|| panic!("row {index} lost"));
        assert!(frame.entries[entry].contains(&row.id));
        let member = frame.entries[entry]
            .members
            .iter()
            .find(|member| member.id == row.id)
            .expect("member");
        assert_eq!(member.position, index as u64);
        assert_eq!(member.timestamp_unix_nanos, row.captured_at_unix_nanos);
    }

    // Text is never rewritten: retained samples are the original event text.
    for entry in &frame.entries {
        let first = &rows[entry.first().position as usize];
        let last = &rows[entry.last().position as usize];
        assert_eq!(entry.first_sample, first.text);
        assert_eq!(entry.last_sample, last.text);
    }
}

#[test]
fn folding_is_off_by_default_and_is_an_identity() {
    assert!(!FoldConfig::default().enabled);
    let rows = mixed_feed(60);
    let frame = fold_rows(FoldConfig::default(), &rows);
    assert_eq!(frame.entries.len(), rows.len());
    assert!(frame.entries.iter().all(|entry| !entry.folded));
    assert!(frame.entries.iter().all(|entry| entry.count() == 1));
    assert_eq!(frame.expand(), ids(&rows));
}

#[test]
fn minimum_run_keeps_short_repeats_unfolded() {
    let rows: Vec<DisplayRow> = (0..3u64)
        .map(|sequence| plain(sequence, &format!("retrying in {}ms", sequence)))
        .collect();

    let strict = fold_rows(
        FoldConfig {
            minimum_run: 4,
            ..folding()
        },
        &rows,
    );
    assert_eq!(strict.entries.len(), 1);
    assert_eq!(strict.entries[0].count(), 3);
    assert!(!strict.entries[0].folded, "3 < minimum run of 4");

    let lenient = fold_rows(
        FoldConfig {
            minimum_run: 3,
            ..folding()
        },
        &rows,
    );
    assert!(lenient.entries[0].folded);
}

/// A single run cannot grow past the cap; it splits without losing events.
#[test]
fn run_length_cap_splits_instead_of_dropping_events() {
    let rows: Vec<DisplayRow> = (0..25u64)
        .map(|sequence| plain(sequence, &format!("flush {} pages", sequence)))
        .collect();
    let frame = fold_rows(
        FoldConfig {
            maximum_run: 10,
            ..folding()
        },
        &rows,
    );

    assert_eq!(frame.entries.len(), 3);
    assert_eq!(frame.entries[0].count(), 10);
    assert_eq!(frame.entries[1].count(), 10);
    assert_eq!(frame.entries[2].count(), 5);
    assert!(frame.entries.iter().all(|entry| entry.count() <= 10));
    assert_eq!(frame.expand(), ids(&rows));
}

/// Retention eviction drops the oldest entries and keeps an exact suffix.
#[test]
fn retention_caps_evict_oldest_entries_deterministically() {
    let rows: Vec<DisplayRow> = (0..200u64)
        .map(|sequence| plain(sequence, &format!("unique event {}", word(sequence))))
        .collect();
    let mut engine = FoldEngine::new(FoldConfig {
        maximum_entries: 12,
        maximum_members: 1_000,
        ..folding()
    });
    engine.extend_rows(&rows);

    let stats = engine.stats();
    assert!(stats.entries <= 12, "{stats:?}");
    assert_eq!(stats.observed_events, 200);
    assert!(stats.evicted_entries >= 188);
    assert_eq!(stats.evicted_entries + stats.entries as u64, 200);

    let retained = engine.expand();
    let expected = &ids(&rows)[rows.len() - retained.len()..];
    assert_eq!(retained, expected, "retention must keep an exact suffix");

    // A member cap evicts too, even when the entry count is comfortable.
    let mut engine = FoldEngine::new(FoldConfig {
        maximum_entries: 4_096,
        maximum_members: 20,
        maximum_run: 5,
        ..folding()
    });
    let flood: Vec<DisplayRow> = (0..300u64)
        .map(|sequence| plain(sequence, &format!("same shape {}", sequence)))
        .collect();
    engine.extend_rows(&flood);
    assert!(
        engine.stats().retained_members <= 20,
        "{:?}",
        engine.stats()
    );
    assert!(engine.stats().evicted_members >= 280);
}

/// Adversarial: nothing ever repeats. Tracked patterns and retained memory must
/// stay flat regardless of stream length.
#[test]
fn high_cardinality_stream_never_grows_unbounded() {
    let config = FoldConfig {
        scope: FoldScope::Lookback(100_000),
        maximum_patterns: 32,
        maximum_entries: 256,
        maximum_members: 512,
        maximum_sample_chars: 64,
        ..folding()
    };
    let mut engine = FoldEngine::new(config);

    let mut sampled = Vec::new();
    for sequence in 0..30_000u64 {
        let text = format!(
            "sess {} verb {} path /q/{}?token=zz{} note {}",
            word(sequence),
            word(sequence * 7 + 1),
            sequence,
            sequence,
            word(sequence * 13 + 3)
        );
        let id = RowId::new(SOURCE, sequence);
        engine.push(FoldEvent {
            id: &id,
            text: &text,
            level: "",
            timestamp_unix_nanos: Some(sequence as i64),
            columns: &[],
        });
        if sequence % 5_000 == 4_999 {
            sampled.push(engine.stats());
        }
    }

    for stats in &sampled {
        assert!(stats.open_patterns <= 32, "{stats:?}");
        assert!(stats.entries <= 256, "{stats:?}");
        assert!(stats.retained_members <= 512, "{stats:?}");
        assert!(stats.retained_bytes <= 256 * 1024, "{stats:?}");
    }
    // Steady state: the footprint at 10k events is within one eviction batch of
    // the footprint at 30k, rather than tracking stream length.
    let smallest = sampled.iter().map(|stats| stats.entries).min().expect("s");
    let largest = sampled.iter().map(|stats| stats.entries).max().expect("s");
    assert!(largest - smallest <= 256 / 4, "{sampled:?}");
    assert!(
        sampled.iter().all(|stats| stats.entries >= 128),
        "{sampled:?}"
    );
    assert_eq!(engine.stats().observed_events, 30_000);
    assert!(engine.entries().iter().all(|entry| !entry.folded));
}

/// Aggressiveness is observable and ordered: each level folds a superset of the
/// previous one.
#[test]
fn normalisation_levels_change_what_folds() {
    let quoted = vec![
        plain(0, "rejected user \"amelie\" from host alpha"),
        plain(1, "rejected user \"bruno\" from host alpha"),
    ];
    let conservative = fold_rows(
        FoldConfig {
            aggressiveness: Normalisation::Conservative,
            minimum_run: 2,
            ..folding()
        },
        &quoted,
    );
    assert_eq!(conservative.entries.len(), 2, "quoted values kept literal");

    let standard = fold_rows(
        FoldConfig {
            minimum_run: 2,
            ..folding()
        },
        &quoted,
    );
    assert_eq!(standard.entries.len(), 1);
    assert!(standard.entries[0].folded);

    let mixed = vec![
        plain(0, "lease lost on worker-7a"),
        plain(1, "lease lost on worker-31b"),
    ];
    let standard = fold_rows(
        FoldConfig {
            minimum_run: 2,
            ..folding()
        },
        &mixed,
    );
    assert_eq!(standard.entries.len(), 2, "digit suffix differs by letter");

    let aggressive = fold_rows(
        FoldConfig {
            aggressiveness: Normalisation::Aggressive,
            minimum_run: 2,
            ..folding()
        },
        &mixed,
    );
    assert_eq!(aggressive.entries.len(), 1);
    assert!(
        aggressive.entries[0].pattern.ends_with("worker-<tok>"),
        "{:?}",
        aggressive.entries[0].pattern
    );
}

#[test]
fn normalisation_rules_apply_in_order() {
    let config = folding();
    assert_eq!(
        pattern_key("2026-09-07T10:22:31.884Z started in 1.5e3 ms", "", &config),
        "<ts> started in <num> ms"
    );
    assert_eq!(
        pattern_key(
            "session 3f2504e0-4f89-11d3-9a0c-0305e82c3301 closed",
            "",
            &config
        ),
        "session <uuid> closed"
    );
    assert_eq!(
        pattern_key("peer 192.168.10.4:5432 at 12:00:01", "", &config),
        "peer <ip>:<num> at <ts>"
    );
    assert_eq!(
        pattern_key("loaded /var/log/lvu/seg-9.bin and ./local.cfg", "", &config),
        "loaded <path> and <path>"
    );
    assert_eq!(
        pattern_key("frame 0xdeadbeef digest 9f86d081884c7d65", "", &config),
        "frame <hex> digest <hex>"
    );
    // A plain decimal token is a number, not a hex identifier.
    assert_eq!(pattern_key("count 12345678", "", &config), "count <num>");
    // Whitespace runs collapse so ragged alignment folds with tight alignment.
    assert_eq!(
        pattern_key("a\t \n b", "", &config),
        pattern_key("a b", "", &config)
    );
    assert_eq!(pattern_key("boom", "ERROR", &config), "ERROR|boom");
}

#[test]
fn keys_and_samples_are_bounded_and_truncated_on_character_boundaries() {
    let config = FoldConfig {
        maximum_key_chars: 24,
        maximum_sample_chars: 8,
        maximum_scan_chars: 40,
        minimum_run: 2,
        ..folding()
    };
    let long = "日本語のログ行がとても長い場合の折りたたみ確認".repeat(20);
    let key = pattern_key(&long, "", &config);
    assert_eq!(character_count(&key), 24);
    assert!(long.starts_with(&key));

    let rows = vec![plain(0, &long), plain(1, &long)];
    let frame = fold_rows(config, &rows);
    assert_eq!(frame.entries.len(), 1);
    assert_eq!(character_count(&frame.entries[0].first_sample), 8);
    assert_eq!(frame.entries[0].first_sample, "日本語のログ行が");
}

#[test]
fn unicode_text_folds_and_never_splits_a_grapheme() {
    // Combining marks stay attached to their base character.
    let combining = "e\u{301}cho\u{301} de\u{301}marrage\u{301}";
    assert_eq!(character_count(combining), 14);
    assert_eq!(truncate_chars(combining, 1), "e\u{301}");
    assert_eq!(truncate_chars(combining, 4), "e\u{301}cho\u{301}");
    assert!(truncate_chars(combining, 4).ends_with('\u{301}'));
    // Wide CJK is counted per character, not per byte.
    assert_eq!(character_count("接続失敗"), 4);
    assert_eq!(truncate_chars("接続失敗", 2), "接続");
    assert_eq!(truncate_chars("接続失敗", 99), "接続失敗");
    assert_eq!(truncate_chars("接続失敗", 0), "");

    // Mixed-script floods with volatile numbers fold on shape.
    let rows: Vec<DisplayRow> = (0..6u64)
        .map(|sequence| {
            plain(
                sequence,
                &format!(
                    "接続に失敗しました re\u{301}essai {} 回目 ⏱ {}ms",
                    sequence,
                    sequence * 3
                ),
            )
        })
        .collect();
    let frame = fold_rows(folding(), &rows);
    assert_eq!(frame.entries.len(), 1);
    assert_eq!(frame.entries[0].count(), 6);
    assert!(frame.entries[0].pattern.contains("re\u{301}essai"));
    assert_eq!(frame.expand(), ids(&rows));

    // Distinct scripts remain distinct patterns.
    let mixed = vec![
        plain(0, "接続に失敗しました 1 回目"),
        plain(1, "接続に成功しました 1 回目"),
    ];
    assert_eq!(
        fold_rows(
            FoldConfig {
                minimum_run: 2,
                ..folding()
            },
            &mixed
        )
        .entries
        .len(),
        2
    );
}

#[test]
fn reset_clears_derived_state_only() {
    let rows = mixed_feed(50);
    let mut engine = FoldEngine::new(folding());
    engine.extend_rows(&rows);
    assert!(engine.stats().observed_events == 50);
    engine.reset();
    assert_eq!(engine.stats(), Default::default());
    assert!(engine.entries().is_empty());

    engine.extend_rows(&rows);
    assert_eq!(engine.frame(), fold_rows(folding(), &rows));
}

// ---------------------------------------------------------------------------
// Folding by a column other than the derived pattern
// ---------------------------------------------------------------------------

/// A row carrying named column values, as an enriched view serves it.
fn with_columns(sequence: u64, text: &str, columns: &[(&str, &str)]) -> DisplayRow {
    let mut row = plain(sequence, text);
    row.fields = columns
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect();
    row
}

fn by(column: &str) -> FoldKey {
    FoldKey::Column(column.to_owned())
}

/// The whole model: the key is one column's value, and rows sharing it fold
/// however different their text is.
#[test]
fn a_column_key_folds_rows_whose_text_differs() {
    let rows: Vec<DisplayRow> = (0..6u64)
        .map(|sequence| {
            with_columns(
                sequence,
                &format!("wholly unrelated wording number {sequence}"),
                &[("service", "shipper")],
            )
        })
        .chain(std::iter::once(with_columns(
            6,
            "wholly unrelated wording number 6",
            &[("service", "indexer")],
        )))
        .collect();

    // On the derived pattern column these all share a shape, so they fold as
    // one; the column key splits them by the value the user asked about.
    let by_pattern = fold_rows(folding(), &rows);
    assert_eq!(by_pattern.entries.len(), 1);

    let frame = fold_rows_by(folding(), by("service"), &rows);
    assert_eq!(frame.entries.len(), 2);
    assert_eq!(frame.entries[0].count(), 6);
    assert_eq!(&*frame.entries[0].pattern, "shipper");
    assert!(frame.entries[0].folded);
    assert_eq!(frame.entries[1].count(), 1);
    assert!(!frame.entries[1].folded);
    // Presentation only: expansion still reproduces the input exactly.
    assert_eq!(frame.expand(), ids(&rows));
}

/// The user said fields that are not included are not replaced. A column key
/// is therefore used verbatim: no timestamps, ids or numbers are substituted,
/// and no level is prefixed, at any aggressiveness.
#[test]
fn a_column_value_is_the_key_verbatim_at_every_aggressiveness() {
    let volatile = "2026-09-07T10:22:31.884Z 10.0.0.7 worker-7a 0xdeadbeef 4711";
    let rows = vec![
        with_columns(0, "first", &[("k", volatile)]),
        with_columns(1, "second", &[("k", volatile)]),
    ];
    for aggressiveness in [
        Normalisation::Conservative,
        Normalisation::Standard,
        Normalisation::Aggressive,
    ] {
        let config = FoldConfig {
            minimum_run: 2,
            aggressiveness,
            ..folding()
        };
        let frame = fold_rows_by(config, by("k"), &rows);
        assert_eq!(frame.entries.len(), 1);
        assert_eq!(&*frame.entries[0].pattern, volatile, "{aggressiveness:?}");
    }

    // Two rows whose values differ only where normalisation would have erased
    // the difference stay apart, which is the point of "not replaced".
    let distinct = vec![
        with_columns(0, "x", &[("k", "attempt 1")]),
        with_columns(1, "x", &[("k", "attempt 2")]),
    ];
    assert_eq!(
        fold_rows_by(
            FoldConfig {
                minimum_run: 2,
                aggressiveness: Normalisation::Aggressive,
                ..folding()
            },
            by("k"),
            &distinct
        )
        .entries
        .len(),
        2
    );

    // The same two rows on the derived column do collapse, so the test above is
    // about the key and not about the rows.
    assert_eq!(
        fold_rows(
            FoldConfig {
                minimum_run: 2,
                ..folding()
            },
            &distinct
        )
        .entries
        .len(),
        1
    );
}

/// A level that separates shapes on the derived column has no say over a
/// column key: the column is the whole key.
#[test]
fn severity_does_not_split_a_column_key() {
    let rows = vec![
        row(0, "INFO", "connected"),
        row(1, "ERROR", "connected"),
        row(2, "WARN", "connected"),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, mut row)| {
        row.fields = vec![("service".into(), "shipper".into())];
        row.id = RowId::new(SOURCE, index as u64);
        row
    })
    .collect::<Vec<_>>();

    assert_eq!(
        fold_rows(
            FoldConfig {
                minimum_run: 2,
                ..folding()
            },
            &rows
        )
        .entries
        .len(),
        3
    );
    let frame = fold_rows_by(
        FoldConfig {
            minimum_run: 2,
            ..folding()
        },
        by("service"),
        &rows,
    );
    assert_eq!(frame.entries.len(), 1);
    assert_eq!(frame.entries[0].count(), 3);
}

/// A row that does not carry the key column has no key. It stays its own
/// visible entry instead of folding together with every other row that is
/// merely missing the same column.
#[test]
fn rows_without_the_key_column_do_not_fold_together() {
    let rows = vec![
        with_columns(0, "a", &[("service", "shipper")]),
        plain(1, "b"),
        plain(2, "c"),
        plain(3, "d"),
        with_columns(4, "e", &[("service", "shipper")]),
    ];
    let frame = fold_rows_by(
        FoldConfig {
            minimum_run: 2,
            ..folding()
        },
        by("service"),
        &rows,
    );
    assert_eq!(frame.entries.len(), 5);
    assert!(frame.entries.iter().all(|entry| !entry.folded));
    assert_eq!(frame.expand(), ids(&rows));

    // An *empty* value is a value: rows the column describes as blank share a
    // key, because that is what the column says about them.
    let blank = vec![
        with_columns(0, "a", &[("service", "")]),
        with_columns(1, "b", &[("service", "")]),
        with_columns(2, "c", &[("service", "")]),
    ];
    let frame = fold_rows_by(
        FoldConfig {
            minimum_run: 2,
            ..folding()
        },
        by("service"),
        &blank,
    );
    assert_eq!(frame.entries.len(), 1);
    assert_eq!(frame.entries[0].count(), 3);
}

/// A column key is capped like every other accumulator, and truncation stays
/// on a character boundary.
#[test]
fn a_column_key_is_truncated_by_characters() {
    let long = "日本語のとても長いサービス名".repeat(40);
    let config = FoldConfig {
        maximum_key_chars: 12,
        minimum_run: 2,
        ..folding()
    };
    let rows = vec![
        with_columns(0, "a", &[("service", &long)]),
        with_columns(1, "b", &[("service", &format!("{long}-tail"))]),
    ];
    let frame = fold_rows_by(config, by("service"), &rows);
    // Two values sharing a long prefix fold once the cap truncates them, and
    // the retained key is exactly the cap.
    assert_eq!(frame.entries.len(), 1);
    assert_eq!(character_count(&frame.entries[0].pattern), 12);
    assert_eq!(&*frame.entries[0].pattern, "日本語のとても長いサービ");
}

/// The prefix-only rule holds for a column key too: how the feed is chopped
/// into arrivals cannot change what folds.
#[test]
fn batch_boundaries_do_not_change_a_column_fold() {
    let rows: Vec<DisplayRow> = (0..200u64)
        .map(|sequence| {
            with_columns(
                sequence,
                &format!("event {sequence}"),
                &[("service", if sequence % 7 < 4 { "a" } else { "b" })],
            )
        })
        .collect();
    let config = FoldConfig {
        minimum_run: 2,
        ..folding()
    };
    let whole = fold_rows_by(config, by("service"), &rows);
    for sizes in [vec![1usize], vec![3, 11], vec![64], vec![7, 1, 40]] {
        let mut engine = FoldEngine::with_key(config, by("service"));
        let mut offset = 0usize;
        let mut cursor = 0usize;
        while offset < rows.len() {
            let take = sizes[cursor % sizes.len()].min(rows.len() - offset);
            engine.extend_rows(&rows[offset..offset + take]);
            offset += take;
            cursor += 1;
        }
        assert_eq!(engine.frame(), whole, "sizes {sizes:?}");
    }
}

/// `FoldKey::Pattern` is the default everywhere, so a caller that says nothing
/// gets exactly the behaviour that existed before a key could be chosen.
#[test]
fn the_default_key_is_the_derived_pattern_column() {
    assert_eq!(FoldKey::default(), FoldKey::Pattern);
    assert_eq!(FoldKey::from_column(None), FoldKey::Pattern);
    assert_eq!(FoldKey::from_column(Some("  ")), FoldKey::Pattern);
    assert_eq!(
        FoldKey::from_column(Some(" service ")),
        FoldKey::Column("service".into())
    );
    assert_eq!(FoldKey::Pattern.column(), None);
    assert_eq!(FoldKey::Column("k".into()).column(), Some("k"));

    let rows = mixed_feed(120);
    assert_eq!(
        FoldEngine::new(folding()).key(),
        &FoldKey::Pattern,
        "new() must stay the pattern engine"
    );
    assert_eq!(
        fold_rows_by(folding(), FoldKey::Pattern, &rows),
        fold_rows(folding(), &rows)
    );
}

/// `fold_key` is the one place a key is derived, and it answers for both kinds.
#[test]
fn fold_key_answers_for_both_kinds_of_key() {
    let config = folding();
    let row = with_columns(
        0,
        "connection to 10.0.0.7 failed",
        &[("service", "shipper")],
    );
    let event = FoldEvent::from(&row);
    assert_eq!(
        fold_key(&event, &FoldKey::Pattern, &config).as_deref(),
        Some(pattern_key(&row.text, &row.level, &config).as_str())
    );
    assert_eq!(
        fold_key(&event, &by("service"), &config).as_deref(),
        Some("shipper")
    );
    assert_eq!(fold_key(&event, &by("absent"), &config), None);
}
