//! What capture costs per byte, and the guard that keeps it that way.
//!
//! The soak measures capture from the outside, through a terminal, mixed with
//! query work; it can say that capture is slow but not where the time goes.
//! This drives the same acquisition and writer path headlessly over a generated
//! file, so a number here is about ingest and nothing else.
//!
//! The assertion is a ratio — bytes per CPU-second — not a wall-clock duration.
//! Wall clock on a shared build box measures the neighbours; CPU seconds
//! measure the work this code actually does, and a regression that doubles the
//! syscalls or the allocations shows up in it whatever else the machine is
//! doing.

mod support;

use lvu_core::{Acquisition, SourceDefinition, SourceId};
use lvu_ingest::{RuntimeConfig, RuntimeState, SourceManager};
use std::{
    io::Write,
    path::Path,
    time::{Duration, Instant},
};

/// Deterministic mixed content in the shape `tests/soak/generate.py` writes:
/// JSON and plain records, a wide Unicode record and a malformed one, so the
/// framer and the journal see realistic line lengths rather than one uniform
/// size that would flatter the per-record cost.
fn generate(path: &Path, target_bytes: u64) -> (u64, u64) {
    let services = ["api", "worker", "ingest", "scheduler"];
    let levels = ["INFO", "INFO", "INFO", "WARN", "ERROR", "DEBUG"];
    let file = std::fs::File::create(path).expect("fixture file");
    let mut out = std::io::BufWriter::with_capacity(1 << 20, file);
    let mut written = 0_u64;
    let mut index = 0_u64;
    // A tiny deterministic PRNG, so the fixture does not depend on a crate and
    // two runs compare byte for byte.
    let mut state = 0x2026_0907_u64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    while written < target_bytes {
        let service = services[(index % 4) as usize];
        let level = levels[(next() % 6) as usize];
        let duration = next() % 4000 + 1;
        let line = if (index + 1).is_multiple_of(1_000) {
            format!("malformed {{ not json and not logfmt {index}")
        } else if index.is_multiple_of(997) {
            format!(
                "{{\"level\":\"{level}\",\"service\":\"{service}\",\"wide\":\"界界é 東京\",\"seq\":{index}}}"
            )
        } else if index.is_multiple_of(3) {
            format!(
                "{{\"level\":\"{level}\",\"service\":\"{service}\",\"seq\":{index},\"request_id\":\"req-{:05}\",\"duration_ms\":{duration},\"message\":\"handled request\"}}",
                index % 5000
            )
        } else {
            format!(
                "{level} service={service} seq={index} request_id=req-{:05} duration_ms={duration} message=handled request",
                index % 5000
            )
        };
        out.write_all(line.as_bytes()).expect("fixture line");
        out.write_all(b"\n").expect("fixture delimiter");
        written += line.len() as u64 + 1;
        index += 1;
    }
    out.flush().expect("fixture flush");
    (index, written)
}

fn file_source(path: &Path) -> SourceDefinition {
    SourceDefinition {
        schema_version: 1,
        id: SourceId::new(),
        name: "throughput fixture".into(),
        acquisition: Acquisition::File {
            path: path.to_owned(),
            follow: false,
        },
        identity_hints: Default::default(),
        retention: None,
    }
}

/// Reads the file the way acquisition reads it and throws the bytes away: no
/// framing, no journal, no durability. The floor any capture is measured
/// against, and the answer to "is the file itself the cost".
async fn read_only(path: &Path, chunk: usize) -> support::Cost {
    use tokio::io::AsyncReadExt;
    let before = support::Usage::now();
    let started = Instant::now();
    let mut file = tokio::fs::File::open(path).await.expect("open fixture");
    let mut buffer = vec![0_u8; chunk];
    let mut read = 0_u64;
    loop {
        let count = file.read(&mut buffer).await.expect("read fixture");
        if count == 0 {
            break;
        }
        read += count as u64;
    }
    support::Cost {
        source_bytes: read,
        records: 0,
        journal_bytes: 0,
        syncs: 0,
        elapsed: started.elapsed(),
        usage: support::Usage::now().since(before),
    }
}

/// Captures the whole file and reports what it cost.
async fn capture(bytes: u64) -> support::Cost {
    capture_with(bytes, RuntimeConfig::default()).await
}

async fn capture_with(bytes: u64, config: RuntimeConfig) -> support::Cost {
    let directory = tempfile::tempdir().expect("temporary root");
    let source = directory.path().join("source.log");
    let (lines, written) = generate(&source, bytes);
    let manager = SourceManager::new(directory.path().join("capture"), config).expect("manager");
    let definition = file_source(&source);

    let before = support::Usage::now();
    let started = Instant::now();
    let handle = manager.start(definition).await.expect("start");
    let mut progress = handle.subscribe();
    // `follow: false` ends the acquisition at end of file, so settling is a
    // terminal state rather than a quiet period the test has to guess at.
    tokio::time::timeout(Duration::from_secs(600), async {
        loop {
            if progress.borrow_and_update().state.is_terminal() {
                return;
            }
            progress.changed().await.expect("progress channel");
        }
    })
    .await
    .expect("capture settled");
    let elapsed = started.elapsed();
    let usage = support::Usage::now().since(before);
    let final_progress = handle.progress();
    assert!(
        !matches!(
            final_progress.state,
            RuntimeState::Error | RuntimeState::Incomplete | RuntimeState::StorageBlocked
        ),
        "capture ended as {:?}: {:?}",
        final_progress.state,
        final_progress.last_error
    );
    // The record count may exceed the line count: a read that ends mid-line
    // emits the partial as its own record before its terminator arrives, which
    // is the documented framing rule. What must hold exactly is that the
    // journal replays the source's bytes — no loss, no duplication, whatever
    // the chunk boundaries did.
    assert!(
        final_progress.records >= lines,
        "captured {} records for {lines} lines",
        final_progress.records
    );
    let replayed = support::replay(&handle, final_progress.records).await;
    let original = std::fs::read(&source).expect("read fixture back");
    assert_eq!(
        replayed.len(),
        original.len(),
        "journal replayed {} bytes for a {} byte source",
        replayed.len(),
        original.len()
    );
    assert!(
        replayed == original,
        "journal did not replay the source bytes"
    );
    // `follow: false` means the acquisition ends on its own, so shutdown may
    // find the source already settled; that is completion, not a failure.
    for (_, stopped) in manager.shutdown().await {
        match stopped {
            Ok(report) => assert!(report.complete, "capture stopped incomplete"),
            Err(lvu_ingest::RuntimeError::NotActive | lvu_ingest::RuntimeError::Closed) => {}
            Err(error) => panic!("stop: {error}"),
        }
    }
    support::Cost {
        source_bytes: written,
        records: final_progress.records,
        journal_bytes: final_progress.journal_bytes,
        syncs: final_progress.syncs,
        elapsed,
        usage,
    }
}

/// The guard. A ratio, so a busy machine cannot fail it and a regression in the
/// work per byte cannot pass it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn capture_sustains_its_throughput_per_cpu_second() {
    let cost = capture(16 * 1024 * 1024).await;
    println!("{}", cost.report());
    let floor = support::THROUGHPUT_FLOOR_BYTES_PER_CPU_SECOND;
    assert!(
        cost.bytes_per_cpu_second() >= floor,
        "capture fell to {:.1} MB per CPU-second, below the {:.1} MB floor\n{}",
        cost.bytes_per_cpu_second() / 1_048_576.0,
        floor / 1_048_576.0,
        cost.report()
    );
    // The durability guard, and the one that matters most: CPU time barely
    // moves when the commit rate explodes, because an fsync is a wait. This
    // catches the regression the group commit exists to prevent.
    assert!(
        cost.syncs_per_mb() <= support::COMMITS_PER_MB_CEILING,
        "capture committed {:.1} times per MB, above the {:.1} ceiling\n{}",
        cost.syncs_per_mb(),
        support::COMMITS_PER_MB_CEILING,
        cost.report()
    );
    assert!(
        cost.journal_overhead_per_record() <= support::JOURNAL_OVERHEAD_CEILING_BYTES,
        "journal overhead grew to {:.1} bytes per record, above the {} byte ceiling\n{}",
        cost.journal_overhead_per_record(),
        support::JOURNAL_OVERHEAD_CEILING_BYTES,
        cost.report()
    );
}

/// The measurement run. Ignored by default: it is I/O on a shared volume, and
/// it exists to produce the phase table, not to gate anything.
/// `cargo test -p lvu-ingest --test throughput -- --ignored --nocapture`
///
/// The table is built by ablation rather than by instrumenting the path,
/// because a timer inside a loop this hot changes what it measures. Each row
/// removes one cost from the same capture over the same fixture, so a row's
/// distance from the one above it is what that cost is worth.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "measurement run; produces the phase table"]
async fn measure_capture_of_a_soak_sized_source() {
    let bytes = 64 * 1024 * 1024;

    // The floor: reading the bytes and doing nothing with them.
    let directory = tempfile::tempdir().expect("temporary root");
    let fixture = directory.path().join("floor.log");
    generate(&fixture, bytes);
    let floor = read_only(
        &fixture,
        RuntimeConfig::default().acquisition.read_chunk_bytes,
    )
    .await;
    drop(directory);

    // Everything except durability: what the framing, the channel and the
    // journal encoding cost when the filesystem is never asked to commit.
    let commitless = RuntimeConfig {
        sync_every_records: u64::MAX,
        sync_interval: Duration::from_secs(86_400),
        ..RuntimeConfig::default()
    };
    let without_commit = capture_with(bytes, commitless).await;

    // The next lever's ceiling. Widening what the pipeline moves at once —
    // reads, the acquisition channel and the writer's drain — is what is left
    // after durability, so this row says what that would be worth. The read
    // size alone changes nothing; the record-at-a-time handoff is the cost.
    let coarse = RuntimeConfig {
        sync_every_records: u64::MAX,
        sync_interval: Duration::from_secs(86_400),
        acquisition: lvu_core::acquisition::CaptureLimits {
            read_chunk_bytes: 1024 * 1024,
            channel_capacity: 4096,
            ..RuntimeConfig::default().acquisition
        },
        writer_queue_capacity: 4096,
        batch_records: 4096,
        ..RuntimeConfig::default()
    };
    let coarse = capture_with(bytes, coarse).await;

    // The policy as it was, reproduced by its measured effect: the old rule
    // committed every eight *batches*, which on this fixture came out at one
    // commit per 417 records. Setting the record bound to that reproduces the
    // rate the scheduler used to produce, against everything else as shipped.
    let old_rate = RuntimeConfig {
        sync_every_records: 417,
        ..RuntimeConfig::default()
    };
    let old_rate = capture_with(bytes, old_rate).await;

    // The shipped policy.
    let shipped = capture(bytes).await;

    println!(
        "\n=== ingest phase table, {} MB source ===",
        bytes / 1_048_576
    );
    for (phase, cost) in [
        ("read only, no framing or journal", floor),
        ("capture without durability", without_commit),
        ("capture without durability, coarse chunks/batches", coarse),
        ("capture at the previous commit rate", old_rate),
        ("capture as shipped", shipped),
    ] {
        println!("\n-- {phase}\n{}", cost.report());
    }
    println!(
        "\ndurability costs {:.2}s wall and {:.2}s CPU over {} commits",
        (shipped.elapsed.as_secs_f64() - without_commit.elapsed.as_secs_f64()).max(0.0),
        (shipped.usage.total().as_secs_f64() - without_commit.usage.total().as_secs_f64()).max(0.0),
        shipped.syncs,
    );
    println!(
        "framing, channel and journal cost {:.2}s wall over the bare read",
        (without_commit.elapsed.as_secs_f64() - floor.elapsed.as_secs_f64()).max(0.0),
    );
}
