use flate2::{Compression, write::GzEncoder};
use lvu_core::{
    Acquisition, ChunkPosition, CommandDefinition, CommandProgram, RestartPolicy, SourceDefinition,
    SourceId, StreamKind,
};
use lvu_ingest::{RuntimeConfig, RuntimeError, RuntimeState, SourceHandle, SourceManager};
use std::{
    collections::BTreeMap,
    fs,
    future::Future,
    io::{self, Write},
    pin::Pin,
    task::{Context, Poll, Waker},
    time::{Duration, Instant},
};
use tempfile::tempdir;
use tokio::io::{AsyncRead, AsyncWriteExt, ReadBuf};

fn gzip_member(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

struct PartialThenError(Option<Vec<u8>>);

impl AsyncRead for PartialThenError {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if let Some(bytes) = self.0.take() {
            buffer.put_slice(&bytes);
            Poll::Ready(Ok(()))
        } else {
            Poll::Ready(Err(io::Error::other("injected reader failure")))
        }
    }
}

fn command_source(id: SourceId, script: &str) -> SourceDefinition {
    SourceDefinition {
        schema_version: 1,
        id,
        name: "fixture command".into(),
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

fn file_source(id: SourceId, path: &std::path::Path, follow: bool) -> SourceDefinition {
    SourceDefinition {
        schema_version: 1,
        id,
        name: "fixture file".into(),
        acquisition: Acquisition::File {
            path: path.to_owned(),
            follow,
        },
        identity_hints: BTreeMap::new(),
        retention: None,
    }
}

fn stdin_source(id: SourceId) -> SourceDefinition {
    SourceDefinition {
        schema_version: 1,
        id,
        name: "stdin session".into(),
        acquisition: Acquisition::Stdin,
        identity_hints: BTreeMap::new(),
        retention: None,
    }
}

fn small_config() -> RuntimeConfig {
    let mut config = RuntimeConfig::default();
    config.acquisition.channel_capacity = 2;
    config.acquisition.read_chunk_bytes = 16;
    config.acquisition.maximum_record_bytes = 32;
    config.acquisition.partial_flush_interval = Duration::from_millis(20);
    config.writer_queue_capacity = 2;
    config.batch_records = 4;
    config.sync_every_batches = 2;
    config.graceful_stop_deadline = Duration::from_secs(3);
    config
}

async fn wait_for(handle: &SourceHandle, predicate: impl Fn(&lvu_ingest::SourceProgress) -> bool) {
    let mut progress = handle.subscribe();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if predicate(&progress.borrow()) {
                return;
            }
            progress
                .changed()
                .await
                .expect("runtime progress channel closed");
        }
    })
    .await
    .expect("runtime state timed out");
}

async fn all_records(handle: &SourceHandle) -> Vec<lvu_core::RawRecord> {
    let mut offset = 0;
    let mut records = Vec::new();
    loop {
        let page = handle.read_page(offset, 3, 1024).await.unwrap();
        records.extend(page.records);
        if page.end_of_journal {
            return records;
        }
        offset = page.next_offset;
    }
}

#[tokio::test]
async fn command_is_journaled_once_and_shared_handles_page_after_exit() {
    let root = tempdir().unwrap();
    let id = SourceId::new();
    let manager = SourceManager::new(root.path(), small_config()).unwrap();
    let handle = manager
        .start(command_source(
            id,
            "printf 'one\\n'; printf '\\377two\\r\\n'; printf 'err\\n' >&2",
        ))
        .await
        .unwrap();
    let second_view = handle.clone();
    wait_for(&handle, |value| value.state == RuntimeState::Stopped).await;
    let first = all_records(&handle).await;
    let second = all_records(&second_view).await;
    assert_eq!(
        first
            .iter()
            .map(|record| (&record.bytes, &record.delimiter, record.stream))
            .collect::<Vec<_>>(),
        second
            .iter()
            .map(|record| (&record.bytes, &record.delimiter, record.stream))
            .collect::<Vec<_>>()
    );
    assert!(first.iter().any(|record| record.bytes == b"\xfftwo"
        && record.delimiter == b"\r\n"
        && record.stream == StreamKind::Stdout));
    assert!(
        first
            .iter()
            .any(|record| record.bytes == b"err" && record.stream == StreamKind::Stderr)
    );
    assert_eq!(handle.progress().records, 3);
    let catalog =
        fs::read_to_string(root.path().join(id.0.to_string()).join("events.jsonl")).unwrap();
    assert!(catalog.contains("\"boundary\""));
    assert!(catalog.contains("\"command_exit\""));
    assert!(catalog.contains("\"stopped\""));
}

#[cfg(unix)]
#[tokio::test]
async fn attached_stdin_pipe_is_durable_exact_and_remains_pageable_after_eof() {
    let root = tempdir().unwrap();
    let id = SourceId::new();
    let definition = stdin_source(id);
    let manager = SourceManager::new(root.path(), small_config()).unwrap();
    let (reader, mut writer) = tokio::net::UnixStream::pair().unwrap();
    let handle = manager.start_with_reader(definition, reader).await.unwrap();
    writer.write_all(b"one\xff\r\npartial").await.unwrap();
    writer.shutdown().await.unwrap();
    wait_for(&handle, |progress| progress.state == RuntimeState::Stopped).await;
    let records = all_records(&handle).await;
    assert!(
        records
            .iter()
            .all(|record| record.stream == StreamKind::Stdin)
    );
    let bytes: Vec<_> = records
        .into_iter()
        .flat_map(|record| record.bytes.into_iter().chain(record.delimiter))
        .collect();
    assert_eq!(bytes, b"one\xff\r\npartial");
    assert_eq!(handle.progress().synced_records, handle.progress().records);
    assert!(
        !handle
            .read_page(0, 1, 1024)
            .await
            .unwrap()
            .records
            .is_empty()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn graceful_stdin_stop_drains_partial_and_closes_an_open_pipe() {
    let root = tempdir().unwrap();
    let manager = SourceManager::new(root.path(), small_config()).unwrap();
    let (reader, mut writer) = tokio::net::UnixStream::pair().unwrap();
    let handle = manager
        .start_with_reader(stdin_source(SourceId::new()), reader)
        .await
        .unwrap();
    writer.write_all(b"line\npart\xfe").await.unwrap();
    wait_for(&handle, |progress| progress.records >= 2).await;
    let report = handle.stop().await.unwrap();
    assert!(report.complete);
    assert_eq!(captured_bytes(&handle).await, b"line\npart\xfe");
    assert_eq!(handle.progress().synced_records, handle.progress().records);
    let closed = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if writer.write_all(&vec![0_u8; 64 * 1024]).await.is_err() {
                break;
            }
        }
    })
    .await;
    assert!(
        closed.is_ok(),
        "stopped stdin reader retained its pipe descriptor"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn stdin_abort_does_not_wait_for_an_open_pipe() {
    let root = tempdir().unwrap();
    let manager = SourceManager::new(root.path(), small_config()).unwrap();
    let (reader, _writer) = tokio::net::UnixStream::pair().unwrap();
    let handle = manager
        .start_with_reader(stdin_source(SourceId::new()), reader)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), handle.abort())
        .await
        .expect("open stdin pipe prevented runtime abort")
        .unwrap();
}

#[tokio::test]
async fn stdin_read_error_durably_flushes_partial_bytes_and_is_not_a_clean_stop() {
    let root = tempdir().unwrap();
    let manager = SourceManager::new(root.path(), small_config()).unwrap();
    let id = SourceId::new();
    let input = b"broken\xffpartial".to_vec();
    let handle = manager
        .start_with_reader(stdin_source(id), PartialThenError(Some(input.clone())))
        .await
        .unwrap();
    wait_for(&handle, |progress| {
        matches!(
            progress.state,
            RuntimeState::Stopped
                | RuntimeState::Aborted
                | RuntimeState::Incomplete
                | RuntimeState::StorageBlocked
                | RuntimeState::Error
        )
    })
    .await;
    let progress = handle.progress();
    assert_eq!(progress.state, RuntimeState::Error);
    assert!(
        progress
            .last_error
            .as_deref()
            .is_some_and(|message| message.contains("injected reader failure"))
    );
    assert_eq!(captured_bytes(&handle).await, input);
    assert_eq!(progress.synced_records, progress.records);
    let catalog =
        fs::read_to_string(root.path().join(id.0.to_string()).join("events.jsonl")).unwrap();
    assert!(catalog.contains("injected reader failure"));
    assert!(!catalog.contains("\"stopped\""));
}

#[tokio::test]
async fn stdin_requires_one_matching_attachment_and_a_fresh_source_identity() {
    let root = tempdir().unwrap();
    let manager = SourceManager::new(root.path(), small_config()).unwrap();
    let id = SourceId::new();
    assert!(matches!(
        manager.start(stdin_source(id)).await,
        Err(RuntimeError::StdinNotAttached)
    ));
    assert!(!root.path().join(id.0.to_string()).exists());

    let (_writer, reader) = tokio::io::duplex(8);
    let file = root.path().join("file.log");
    fs::write(&file, b"").unwrap();
    assert!(matches!(
        manager
            .start_with_reader(file_source(SourceId::new(), &file, false), reader)
            .await,
        Err(RuntimeError::ReaderAttachmentMismatch)
    ));

    let (writer, reader) = tokio::io::duplex(8);
    let first = manager
        .start_with_reader(stdin_source(id), reader)
        .await
        .unwrap();
    let (_duplicate_writer, duplicate_reader) = tokio::io::duplex(8);
    assert!(matches!(
        manager
            .start_with_reader(stdin_source(id), duplicate_reader)
            .await,
        Err(RuntimeError::AlreadyRunning)
    ));
    drop(writer);
    wait_for(&first, |progress| progress.state == RuntimeState::Stopped).await;
    let (_replay_writer, replay_reader) = tokio::io::duplex(8);
    let replay = manager
        .start_with_reader(stdin_source(id), replay_reader)
        .await;
    assert!(
        matches!(&replay, Err(RuntimeError::StdinAlreadyCaptured)),
        "replay result: {:?}",
        replay.err()
    );
}

#[tokio::test]
async fn partial_command_is_published_before_exit_and_crlf_rejoins_exactly() {
    let root = tempdir().unwrap();
    let id = SourceId::new();
    let manager = SourceManager::new(root.path(), small_config()).unwrap();
    let handle = manager
        .start(command_source(
            id,
            "printf partial; sleep 0.1; printf '\\r\\n'; sleep 1",
        ))
        .await
        .unwrap();
    wait_for(&handle, |value| value.records >= 1).await;
    assert_eq!(handle.progress().state, RuntimeState::Running);
    let early = handle.read_page(0, 10, 1024).await.unwrap();
    assert_eq!(early.records[0].bytes, b"partial");
    assert_eq!(early.records[0].chunk, ChunkPosition::Start);
    wait_for(&handle, |value| value.records >= 2).await;
    let completed_line = handle.read_page(0, 10, 1024).await.unwrap();
    let before_exit: Vec<_> = completed_line
        .records
        .iter()
        .flat_map(|record| record.bytes.iter().chain(record.delimiter.iter()))
        .copied()
        .collect();
    assert_eq!(before_exit, b"partial\r\n");
    let report = handle.stop().await.unwrap();
    assert!(report.complete);
    let records = all_records(&handle).await;
    let reconstructed: Vec<_> = records
        .iter()
        .flat_map(|record| record.bytes.iter().chain(record.delimiter.iter()))
        .copied()
        .collect();
    assert_eq!(reconstructed, b"partial\r\n");
    assert_eq!(records.last().unwrap().chunk, ChunkPosition::End);
}

#[tokio::test]
async fn graceful_stop_drains_saturated_queues_and_delayed_writer() {
    let root = tempdir().unwrap();
    let path = root.path().join("burst.log");
    let fixture: Vec<u8> = (0..40)
        .flat_map(|index| format!("{index:02}\n").into_bytes())
        .collect();
    fs::write(&path, &fixture).unwrap();
    let mut config = small_config();
    config.acquisition.channel_capacity = 1;
    config.writer_queue_capacity = 1;
    config.batch_records = 1;
    config.writer_delay = Duration::from_millis(2);
    config.acquisition.read_chunk_bytes = fixture.len();
    let manager = SourceManager::new(root.path().join("capture"), config).unwrap();
    let handle = manager
        .start(file_source(SourceId::new(), &path, true))
        .await
        .unwrap();
    wait_for(&handle, |value| value.records >= 1).await;
    let report = handle.stop().await.unwrap();
    assert!(report.complete);
    let bytes: Vec<_> = all_records(&handle)
        .await
        .into_iter()
        .flat_map(|record| record.bytes.into_iter().chain(record.delimiter))
        .collect();
    assert_eq!(bytes, fixture);
    assert_eq!(handle.progress().state, RuntimeState::Stopped);
}

#[tokio::test]
async fn abort_reports_unpublished_partial_bytes() {
    let root = tempdir().unwrap();
    let mut config = small_config();
    config.acquisition.partial_flush_interval = Duration::from_secs(30);
    let manager = SourceManager::new(root.path(), config).unwrap();
    let handle = manager
        .start(command_source(SourceId::new(), "printf pending; sleep 30"))
        .await
        .unwrap();
    wait_for(&handle, |value| value.boundaries == 1).await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    let report = handle.abort().await.unwrap();
    assert_eq!(report.discarded_bytes, 7);
    assert!(!report.discarded_bytes_known);
    assert_eq!(handle.progress().state, RuntimeState::Aborted);
    assert_eq!(handle.progress().discarded_bytes, 7);
}

#[tokio::test]
async fn graceful_stop_deadline_reports_incomplete_without_false_success() {
    let root = tempdir().unwrap();
    let path = root.path().join("slow.log");
    fs::write(&path, b"one\ntwo\nthree\n").unwrap();
    let mut config = small_config();
    config.writer_queue_capacity = 1;
    config.batch_records = 1;
    config.writer_delay = Duration::from_millis(200);
    config.graceful_stop_deadline = Duration::from_millis(40);
    config.acquisition.read_chunk_bytes = 64;
    let manager = SourceManager::new(root.path().join("capture"), config).unwrap();
    let handle = manager
        .start(file_source(SourceId::new(), &path, true))
        .await
        .unwrap();
    wait_for(&handle, |value| value.records >= 1).await;
    let started = Instant::now();
    let report = handle.stop().await.unwrap();
    assert!(!report.complete);
    assert!(!report.discarded_bytes_known);
    assert!(started.elapsed() < Duration::from_millis(150));
    wait_for(&handle, |value| value.state == RuntimeState::Incomplete).await;
}

#[tokio::test]
async fn long_invalid_file_round_trips_through_runtime_fragments() {
    let root = tempdir().unwrap();
    let path = root.path().join("binary.log");
    let mut fixture = vec![0xff];
    fixture.extend((0..100).map(|value| b'a' + value % 26));
    fs::write(&path, &fixture).unwrap();
    let manager = SourceManager::new(root.path().join("capture"), small_config()).unwrap();
    let handle = manager
        .start(file_source(SourceId::new(), &path, false))
        .await
        .unwrap();
    wait_for(&handle, |value| value.state == RuntimeState::Stopped).await;
    let records = all_records(&handle).await;
    assert_eq!(records.first().unwrap().chunk, ChunkPosition::Start);
    assert_eq!(records.last().unwrap().chunk, ChunkPosition::End);
    let captured: Vec<_> = records
        .into_iter()
        .flat_map(|record| record.bytes.into_iter().chain(record.delimiter))
        .collect();
    assert_eq!(captured, fixture);
}

#[tokio::test]
async fn restart_preserves_ids_generation_and_single_writer() {
    let root = tempdir().unwrap();
    let id = SourceId::new();
    let manager = SourceManager::new(root.path(), small_config()).unwrap();
    let definition = command_source(id, "printf 'record\\n'");
    let first = manager.start(definition.clone()).await.unwrap();
    assert!(matches!(
        manager.start(definition.clone()).await,
        Err(RuntimeError::AlreadyRunning)
    ));
    wait_for(&first, |value| value.state == RuntimeState::Stopped).await;
    let first_id = all_records(&first).await[0].record_id;
    let second = manager.start(definition).await.unwrap();
    wait_for(&second, |value| value.state == RuntimeState::Stopped).await;
    let records = all_records(&second).await;
    assert_eq!(second.progress().generation, 2);
    assert_eq!(records.len(), 2);
    assert!(records[1].record_id.sequence > first_id.sequence);
}

#[tokio::test]
async fn independent_managers_cannot_duplicate_a_source_writer() {
    let root = tempdir().unwrap();
    let id = SourceId::new();
    let first_manager = SourceManager::new(root.path(), small_config()).unwrap();
    let second_manager = SourceManager::new(root.path(), small_config()).unwrap();
    let definition = command_source(id, "sleep 30");
    let first = first_manager.start(definition.clone()).await.unwrap();
    assert!(matches!(
        second_manager.start(definition).await,
        Err(RuntimeError::AlreadyRunning)
    ));
    first.abort().await.unwrap();
}

#[tokio::test]
async fn unsupported_acquisition_modes_fail_before_creating_capture_state() {
    let root = tempdir().unwrap();
    let manager = SourceManager::new(root.path(), small_config()).unwrap();
    let schema_id = SourceId::new();
    let mut unsupported_schema = command_source(schema_id, "true");
    unsupported_schema.schema_version = 2;
    assert!(matches!(
        manager.start(unsupported_schema).await,
        Err(RuntimeError::DefinitionUnsupported)
    ));
    assert!(!root.path().join(schema_id.0.to_string()).exists());
}

#[tokio::test]
async fn manager_shutdown_gracefully_stops_each_active_source() {
    let root = tempdir().unwrap();
    let first_path = root.path().join("first.log");
    let second_path = root.path().join("second.log");
    fs::write(&first_path, b"first\n").unwrap();
    fs::write(&second_path, b"second\n").unwrap();
    let manager = SourceManager::new(root.path().join("capture"), small_config()).unwrap();
    let first = manager
        .start(file_source(SourceId::new(), &first_path, true))
        .await
        .unwrap();
    let second = manager
        .start(file_source(SourceId::new(), &second_path, true))
        .await
        .unwrap();
    let reports = manager.shutdown().await;
    assert_eq!(reports.len(), 2);
    assert!(
        reports
            .iter()
            .all(|(_, report)| report.as_ref().unwrap().complete)
    );
    assert_eq!(first.progress().state, RuntimeState::Stopped);
    assert_eq!(second.progress().state, RuntimeState::Stopped);
}

#[tokio::test]
async fn bounded_page_traffic_and_live_capture_make_progress_together() {
    let root = tempdir().unwrap();
    let mut config = small_config();
    config.writer_queue_capacity = 2;
    config.batch_records = 1;
    config.writer_delay = Duration::from_millis(1);
    let manager = SourceManager::new(root.path(), config).unwrap();
    let script = "i=0; while [ $i -lt 100 ]; do echo $i; i=$((i+1)); done";
    let handle = manager
        .start(command_source(SourceId::new(), script))
        .await
        .unwrap();
    for _ in 0..20 {
        let page = tokio::time::timeout(Duration::from_secs(1), handle.read_page(0, 2, 128))
            .await
            .expect("page request starved")
            .unwrap();
        assert!(page.records.len() <= 2);
        if handle.progress().state == RuntimeState::Stopped {
            break;
        }
    }
    wait_for(&handle, |value| value.state == RuntimeState::Stopped).await;
    assert!(handle.progress().records >= 100);
    let expected: Vec<_> = (0..100)
        .flat_map(|value| format!("{value}\n").into_bytes())
        .collect();
    let captured: Vec<_> = all_records(&handle)
        .await
        .into_iter()
        .filter(|record| record.stream == StreamKind::Stdout)
        .flat_map(|record| record.bytes.into_iter().chain(record.delimiter))
        .collect();
    assert_eq!(captured, expected);
}

#[tokio::test]
async fn storage_limit_stops_capture_without_eviction_and_can_recover() {
    let root = tempdir().unwrap();
    let id = SourceId::new();
    let mut limited = small_config();
    limited.storage_limit_bytes = Some(90);
    let manager = SourceManager::new(root.path(), limited).unwrap();
    let mut definition = command_source(
        id,
        "if [ -e rerun ]; then printf 'recovered\\n'; else touch rerun; echo $$ > source.pid; printf 'first-line\\nsecond-line\\n'; sleep 30; fi",
    );
    if let Acquisition::Command { command } = &mut definition.acquisition {
        command.cwd = Some(root.path().to_owned());
    }
    let handle = manager.start(definition.clone()).await.unwrap();
    wait_for(&handle, |value| value.state == RuntimeState::StorageBlocked).await;
    assert!(handle.progress().records <= 1);
    assert!(!handle.progress().discarded_bytes_known);
    assert!(handle.progress().discarded_bytes > 0);
    let process_id = fs::read_to_string(root.path().join("source.pid"))
        .unwrap()
        .trim()
        .parse::<u32>()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while std::path::Path::new(&format!("/proc/{process_id}")).exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("storage-blocked source process leaked");
    drop(manager);

    let recovered = SourceManager::new(root.path(), small_config()).unwrap();
    let handle = recovered.start(definition).await.unwrap();
    wait_for(&handle, |value| value.state == RuntimeState::Stopped).await;
    assert!(
        all_records(&handle)
            .await
            .iter()
            .any(|record| record.bytes == b"recovered")
    );
}

#[tokio::test]
async fn queued_page_falls_back_across_storage_terminal_transition() {
    let root = tempdir().unwrap();
    let input = root.path().join("input.log");
    fs::write(&input, b"first-line\nsecond-line\n").unwrap();
    let mut config = small_config();
    config.batch_records = 1;
    config.writer_queue_capacity = 2;
    config.writer_delay = Duration::from_millis(200);
    config.storage_limit_bytes = Some(150);
    let manager = SourceManager::new(root.path().join("data"), config).unwrap();
    let handle = manager
        .start(file_source(SourceId::new(), &input, false))
        .await
        .unwrap();
    wait_for(&handle, |progress| progress.records == 1).await;

    let page = tokio::time::timeout(Duration::from_secs(2), handle.read_page(0, 10, 1024))
        .await
        .expect("queued page did not resolve across writer shutdown")
        .expect("terminal page read did not use the journal fallback");
    assert_eq!(page.records.len(), 1);
    assert_eq!(page.records[0].bytes, b"first-line");
    wait_for(&handle, |progress| {
        progress.state == RuntimeState::StorageBlocked
    })
    .await;
}

#[tokio::test]
async fn gzip_reopen_is_durable_and_abort_tail_reconciliation_does_not_duplicate() {
    let root = tempdir().unwrap();
    let path = root.path().join("extensionless-archive");
    let mut input = b"one\xff\r\n".to_vec();
    for index in 0..100 {
        input.extend_from_slice(format!("line-{index}\n").as_bytes());
    }
    fs::write(&path, gzip_member(&input)).unwrap();
    let id = SourceId::new();
    let definition = file_source(id, &path, true);

    let mut first_config = small_config();
    first_config.batch_records = 1;
    first_config.writer_delay = Duration::from_millis(5);
    let manager = SourceManager::new(root.path().join("capture"), first_config).unwrap();
    let first = manager.start(definition.clone()).await.unwrap();
    wait_for(&first, |progress| progress.records > 0).await;
    first.abort().await.unwrap();
    wait_for(&first, |progress| progress.state == RuntimeState::Aborted).await;
    let count_after_abort = first.progress().records;
    drop(manager);

    let manager = SourceManager::new(root.path().join("capture"), small_config()).unwrap();
    let reopened = manager.start(definition.clone()).await.unwrap();
    wait_for(&reopened, |progress| {
        progress.state == RuntimeState::Stopped
    })
    .await;
    assert_eq!(captured_bytes(&reopened).await, input);
    let reopened_count = reopened.progress().records;
    assert!(reopened.progress().records >= count_after_abort);
    drop(manager);

    let manager = SourceManager::new(root.path().join("capture"), small_config()).unwrap();
    let unchanged = manager.start(definition).await.unwrap();
    wait_for(&unchanged, |progress| {
        progress.state == RuntimeState::Stopped
    })
    .await;
    assert_eq!(unchanged.progress().records, reopened_count);
    assert_eq!(captured_bytes(&unchanged).await, input);
}

#[tokio::test]
async fn changed_gzip_is_rejected_without_duplicate_raw_records() {
    let root = tempdir().unwrap();
    let path = root.path().join("archive.gz");
    fs::write(&path, gzip_member(b"original\n")).unwrap();
    let id = SourceId::new();
    let definition = file_source(id, &path, false);
    let capture_root = root.path().join("capture");
    let manager = SourceManager::new(&capture_root, small_config()).unwrap();
    let first = manager.start(definition.clone()).await.unwrap();
    wait_for(&first, |progress| progress.state == RuntimeState::Stopped).await;
    assert_eq!(first.progress().records, 1);
    drop(manager);

    let mut appended = gzip_member(b"original\n");
    appended.extend(gzip_member(b"appended\n"));
    fs::write(&path, appended).unwrap();
    let manager = SourceManager::new(&capture_root, small_config()).unwrap();
    let changed = manager.start(definition).await.unwrap();
    wait_for(&changed, |progress| progress.state == RuntimeState::Error).await;
    assert_eq!(changed.progress().records, 1);
    assert_eq!(captured_bytes(&changed).await, b"original\n");
    assert!(
        changed
            .progress()
            .last_error
            .as_deref()
            .is_some_and(|message| message.contains("gzip archive changed"))
    );
}

#[tokio::test]
async fn gzip_graceful_stop_drains_only_the_accepted_prefix_before_success() {
    let root = tempdir().unwrap();
    let path = root.path().join("graceful.gz");
    let input = (0..1000)
        .flat_map(|index| format!("record-{index}\n").into_bytes())
        .collect::<Vec<_>>();
    fs::write(&path, gzip_member(&input)).unwrap();
    let mut config = small_config();
    config.batch_records = 1;
    config.writer_delay = Duration::from_millis(1);
    let manager = SourceManager::new(root.path().join("capture"), config).unwrap();
    let handle = manager
        .start(file_source(SourceId::new(), &path, true))
        .await
        .unwrap();
    wait_for(&handle, |progress| progress.records > 0).await;
    let report = handle.stop().await.unwrap();
    assert!(report.complete);
    assert_eq!(handle.progress().state, RuntimeState::Stopped);
    assert_eq!(handle.progress().last_error, None);
    assert_eq!(handle.progress().records, handle.progress().synced_records);
    let captured = captured_bytes(&handle).await;
    assert!(!captured.is_empty());
    assert!(captured.len() < input.len());
    assert!(input.starts_with(&captured));
}

#[tokio::test]
async fn gzip_storage_limit_keeps_decoded_prefix_and_reports_blocked() {
    let root = tempdir().unwrap();
    let path = root.path().join("limited");
    fs::write(&path, gzip_member(&vec![b'x'; 4096])).unwrap();
    let mut config = small_config();
    config.storage_limit_bytes = Some(512);
    let manager = SourceManager::new(root.path().join("capture"), config).unwrap();
    let handle = manager
        .start(file_source(SourceId::new(), &path, false))
        .await
        .unwrap();
    wait_for(&handle, |progress| {
        progress.state == RuntimeState::StorageBlocked
    })
    .await;
    assert!(handle.progress().discarded_bytes > 0);
    assert!(!handle.progress().discarded_bytes_known);
    assert!(captured_bytes(&handle).await.len() < 4096);
}

#[tokio::test]
async fn legacy_plain_cursor_without_encoding_remains_compatible() {
    let root = tempdir().unwrap();
    let path = root.path().join("plain.log");
    fs::write(&path, b"legacy\n").unwrap();
    let id = SourceId::new();
    let definition = file_source(id, &path, false);
    let capture_root = root.path().join("capture");
    let manager = SourceManager::new(&capture_root, small_config()).unwrap();
    let first = manager.start(definition.clone()).await.unwrap();
    wait_for(&first, |progress| progress.state == RuntimeState::Stopped).await;
    drop(manager);

    let cursor_path = capture_root.join(id.0.to_string()).join("file-cursor.json");
    let mut cursor: serde_json::Value =
        serde_json::from_slice(&fs::read(&cursor_path).unwrap()).unwrap();
    cursor.as_object_mut().unwrap().remove("encoding");
    fs::write(&cursor_path, serde_json::to_vec(&cursor).unwrap()).unwrap();

    let manager = SourceManager::new(&capture_root, small_config()).unwrap();
    let reopened = manager.start(definition).await.unwrap();
    wait_for(&reopened, |progress| {
        progress.state == RuntimeState::Stopped
    })
    .await;
    assert_eq!(reopened.progress().records, 1);
    assert_eq!(captured_bytes(&reopened).await, b"legacy\n");
}

#[tokio::test]
async fn file_reopen_resumes_unchanged_then_captures_only_append() {
    let root = tempdir().unwrap();
    let capture = root.path().join("capture");
    let input = root.path().join("input.log");
    fs::write(&input, b"one\npartial").unwrap();
    let id = SourceId::new();
    let definition = file_source(id, &input, false);

    let first_manager = SourceManager::new(&capture, small_config()).unwrap();
    let first = first_manager.start(definition.clone()).await.unwrap();
    wait_for(&first, |progress| progress.state == RuntimeState::Stopped).await;
    let first_count = first.progress().records;
    assert_eq!(captured_bytes(&first).await, b"one\npartial");
    drop(first_manager);

    let unchanged_manager = SourceManager::new(&capture, small_config()).unwrap();
    let unchanged = unchanged_manager.start(definition.clone()).await.unwrap();
    wait_for(&unchanged, |progress| {
        progress.state == RuntimeState::Stopped
    })
    .await;
    assert_eq!(unchanged.progress().records, first_count);
    assert_eq!(captured_bytes(&unchanged).await, b"one\npartial");
    drop(unchanged_manager);

    fs::OpenOptions::new()
        .append(true)
        .open(&input)
        .unwrap()
        .write_all(b"-continued\r\n")
        .unwrap();
    let appended_manager = SourceManager::new(&capture, small_config()).unwrap();
    let appended = appended_manager.start(definition).await.unwrap();
    wait_for(&appended, |progress| {
        progress.state == RuntimeState::Stopped
    })
    .await;
    assert_eq!(appended.progress().records, first_count + 1);
    assert_eq!(
        captured_bytes(&appended).await,
        b"one\npartial-continued\r\n"
    );
}

#[tokio::test]
async fn graceful_follow_stop_checkpoints_partial_fragment_for_restart() {
    let root = tempdir().unwrap();
    let capture = root.path().join("capture");
    let input = root.path().join("follow.log");
    fs::write(&input, b"partial").unwrap();
    let id = SourceId::new();
    let definition = file_source(id, &input, true);

    let manager = SourceManager::new(&capture, small_config()).unwrap();
    let first = manager.start(definition.clone()).await.unwrap();
    wait_for(&first, |progress| progress.records == 1).await;
    assert!(first.stop().await.unwrap().complete);
    let first_count = first.progress().records;
    drop(manager);

    let manager = SourceManager::new(&capture, small_config()).unwrap();
    let unchanged = manager.start(definition.clone()).await.unwrap();
    wait_for(&unchanged, |progress| progress.boundaries >= 1).await;
    assert!(unchanged.stop().await.unwrap().complete);
    assert_eq!(unchanged.progress().records, first_count);
    drop(manager);

    fs::OpenOptions::new()
        .append(true)
        .open(&input)
        .unwrap()
        .write_all(b"-tail\n")
        .unwrap();
    let manager = SourceManager::new(&capture, small_config()).unwrap();
    let resumed = manager.start(definition).await.unwrap();
    wait_for(&resumed, |progress| progress.records > first_count).await;
    assert!(resumed.stop().await.unwrap().complete);
    assert_eq!(captured_bytes(&resumed).await, b"partial-tail\n");
}

#[tokio::test]
async fn file_resume_detects_rotation_and_truncation_without_skipping_new_bytes() {
    let root = tempdir().unwrap();
    let capture = root.path().join("capture");
    let input = root.path().join("input.log");
    fs::write(&input, b"old\n").unwrap();
    let id = SourceId::new();
    let definition = file_source(id, &input, false);
    let manager = SourceManager::new(&capture, small_config()).unwrap();
    let first = manager.start(definition.clone()).await.unwrap();
    wait_for(&first, |progress| progress.state == RuntimeState::Stopped).await;
    drop(manager);

    fs::rename(&input, root.path().join("rotated.log")).unwrap();
    fs::write(&input, b"new\xff\n").unwrap();
    let manager = SourceManager::new(&capture, small_config()).unwrap();
    let rotated = manager.start(definition.clone()).await.unwrap();
    wait_for(&rotated, |progress| progress.state == RuntimeState::Stopped).await;
    assert_eq!(captured_bytes(&rotated).await, b"old\nnew\xff\n");
    drop(manager);

    fs::write(&input, b"NEW\xff\n").unwrap();
    let manager = SourceManager::new(&capture, small_config()).unwrap();
    let rewritten = manager.start(definition.clone()).await.unwrap();
    wait_for(&rewritten, |progress| {
        progress.state == RuntimeState::Stopped
    })
    .await;
    assert_eq!(captured_bytes(&rewritten).await, b"old\nnew\xff\nNEW\xff\n");
    drop(manager);

    fs::write(&input, b"tiny\n").unwrap();
    let manager = SourceManager::new(&capture, small_config()).unwrap();
    let truncated = manager.start(definition).await.unwrap();
    wait_for(&truncated, |progress| {
        progress.state == RuntimeState::Stopped
    })
    .await;
    assert_eq!(
        captured_bytes(&truncated).await,
        b"old\nnew\xff\nNEW\xff\ntiny\n"
    );
}

#[tokio::test]
async fn stale_cursor_recovers_committed_journal_tail_and_future_cursor_is_preserved() {
    let root = tempdir().unwrap();
    let capture = root.path().join("capture");
    let input = root.path().join("input.log");
    fs::write(&input, b"one\ntwo\n").unwrap();
    let id = SourceId::new();
    let definition = file_source(id, &input, false);
    let manager = SourceManager::new(&capture, small_config()).unwrap();
    let first = manager.start(definition.clone()).await.unwrap();
    wait_for(&first, |progress| progress.state == RuntimeState::Stopped).await;
    let records = all_records(&first).await;
    drop(manager);

    let directory = capture.join(id.0.to_string());
    let cursor_path = directory.join("file-cursor.json");
    let mut cursor: serde_json::Value =
        serde_json::from_slice(&fs::read(&cursor_path).unwrap()).unwrap();
    let mut reader = lvu_core::JournalReader::open(directory.join("capture.journal"), id).unwrap();
    let first_page = reader.read_page(0, 1, 1024).unwrap();
    cursor["journal_offset"] = first_page.next_offset.into();
    cursor["acquisition_id"] = records[0].acquisition_id.to_string().into();
    cursor["file"]["offset"] = 4_u64.into();
    cursor["file"]["evidence"] = serde_json::json!([111, 110, 101, 10]);
    cursor["file"]["content_crc32"] = u64::from(test_crc32(b"one\n")).into();
    fs::write(&cursor_path, serde_json::to_vec(&cursor).unwrap()).unwrap();

    let recovered_manager = SourceManager::new(&capture, small_config()).unwrap();
    let recovered = recovered_manager.start(definition.clone()).await.unwrap();
    wait_for(&recovered, |progress| {
        progress.state == RuntimeState::Stopped
    })
    .await;
    assert_eq!(recovered.progress().records, 2);
    assert_eq!(captured_bytes(&recovered).await, b"one\ntwo\n");
    drop(recovered_manager);

    let mut future: serde_json::Value =
        serde_json::from_slice(&fs::read(&cursor_path).unwrap()).unwrap();
    future["schema_version"] = 999_u64.into();
    let original = serde_json::to_vec(&future).unwrap();
    fs::write(&cursor_path, &original).unwrap();
    let journal_before = fs::read(directory.join("capture.journal")).unwrap();
    let metadata_before = fs::read(directory.join("source.json")).unwrap();
    let rejected = SourceManager::new(&capture, small_config()).unwrap();
    assert!(rejected.start(definition.clone()).await.is_err());
    assert_eq!(fs::read(&cursor_path).unwrap(), original);
    assert_eq!(
        fs::read(directory.join("capture.journal")).unwrap(),
        journal_before
    );
    assert_eq!(
        fs::read(directory.join("source.json")).unwrap(),
        metadata_before
    );

    let malformed = b"{\"schema_version\":".to_vec();
    fs::write(&cursor_path, &malformed).unwrap();
    let rejected = SourceManager::new(&capture, small_config()).unwrap();
    assert!(rejected.start(definition).await.is_err());
    assert_eq!(fs::read(&cursor_path).unwrap(), malformed);
    assert_eq!(
        fs::read(directory.join("capture.journal")).unwrap(),
        journal_before
    );
}

/// Shutting down while a source is finishing by itself must not be reported as
/// a failure to stop it.
///
/// `shutdown` picks what to stop from a snapshot, and a command that exits on
/// its own can become terminal between that snapshot and the stop. Its
/// supervisor is gone by then, so the stop is refused — but the source did
/// stop, and reporting the refusal made the application exit non-zero for a
/// short command that happened to finish as the user quit. Repeated because it
/// is a race: on an idle machine the stop usually wins.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutting_down_a_source_that_is_already_finishing_reports_completion() {
    for attempt in 0..40 {
        let root = tempdir().unwrap();
        let id = SourceId::new();
        let manager =
            SourceManager::new(root.path().join("capture"), RuntimeConfig::default()).unwrap();
        let handle = manager
            .start(command_source(id, "printf 'one line\n'"))
            .await
            .unwrap();
        // Wait only for the record, never for the terminal state: the point is
        // to shut down while the source is ending, which is the window the bug
        // lived in.
        wait_for(&handle, |progress| progress.records >= 1).await;
        for (source, report) in manager.shutdown().await {
            let report = report.unwrap_or_else(|error| {
                panic!("attempt {attempt}: shutdown reported {source:?} as {error}")
            });
            assert!(
                report.complete,
                "attempt {attempt}: a command that ran to its own end reported incomplete"
            );
        }
    }
}

#[tokio::test]
async fn stale_cursor_never_blesses_a_rewritten_acknowledged_prefix() {
    let root = tempdir().unwrap();
    let capture = root.path().join("capture");
    let input = root.path().join("follow.log");
    fs::write(&input, b"abc").unwrap();
    let id = SourceId::new();
    let definition = file_source(id, &input, true);
    let manager = SourceManager::new(&capture, small_config()).unwrap();
    let handle = manager.start(definition.clone()).await.unwrap();
    wait_for(&handle, |progress| progress.records == 1).await;
    let cursor_path = capture.join(id.0.to_string()).join("file-cursor.json");
    let old_cursor = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Ok(bytes) = fs::read(&cursor_path)
                && let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes)
                && value["file"]["offset"] == 3
            {
                break bytes;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("partial checkpoint was not persisted");

    fs::OpenOptions::new()
        .append(true)
        .open(&input)
        .unwrap()
        .write_all(b"tail\n")
        .unwrap();
    wait_for(&handle, |progress| progress.records >= 2).await;
    assert!(handle.stop().await.unwrap().complete);
    drop(manager);

    fs::write(&cursor_path, &old_cursor).unwrap();
    fs::write(&input, b"XYZtail\n").unwrap();
    let manager = SourceManager::new(&capture, small_config()).unwrap();
    let reopened = manager.start(definition).await.unwrap();
    wait_for(&reopened, |progress| progress.records >= 3).await;
    assert!(reopened.stop().await.unwrap().complete);
    assert_eq!(captured_bytes(&reopened).await, b"abctail\nXYZtail\n");
}

#[cfg(unix)]
#[tokio::test]
async fn clean_large_cursor_skips_writer_scan_and_manager_start_cancels_promptly() {
    use std::os::unix::fs::MetadataExt;

    let root = tempdir().unwrap();
    let capture = root.path().join("capture");
    let input = root.path().join("sparse.log");
    let file = fs::File::create(&input).unwrap();
    file.set_len(1024 * 1024 * 1024).unwrap();
    let file_metadata = file.metadata().unwrap();
    let id = SourceId::new();
    let definition = file_source(id, &input, false);
    let directory = capture.join(id.0.to_string());
    fs::create_dir_all(&directory).unwrap();
    fs::write(
        directory.join("source.json"),
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "source_id": id,
            "generation": 1,
            "definition": definition,
        }))
        .unwrap(),
    )
    .unwrap();
    let journal_path = directory.join("capture.journal");
    let (journal, _) = lvu_core::Journal::open(&journal_path, id).unwrap();
    let journal_offset = journal.end_offset().unwrap();
    drop(journal);
    fs::write(
        directory.join("file-cursor.json"),
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "source_id": id,
            "path": input,
            "acquisition_id": uuid::Uuid::new_v4(),
            "journal_offset": journal_offset,
            "file": {
                "offset": file_metadata.len(),
                "identity": {
                    "device": file_metadata.dev(),
                    "inode": file_metadata.ino(),
                },
                "evidence": vec![0_u8; 4096],
                "content_crc32": 0,
            }
        }))
        .unwrap(),
    )
    .unwrap();

    let manager = SourceManager::new(&capture, small_config()).unwrap();
    let mut start = Box::pin(manager.start(file_source(id, &input, false)));
    assert!(matches!(
        start.as_mut().poll(&mut Context::from_waker(Waker::noop())),
        Poll::Pending
    ));
    drop(start);
    tokio::time::timeout(Duration::from_secs(1), manager.shutdown())
        .await
        .expect("manager cancellation waited for a duplicate full-prefix writer scan");
}

async fn captured_bytes(handle: &SourceHandle) -> Vec<u8> {
    all_records(handle)
        .await
        .into_iter()
        .flat_map(|record| record.bytes.into_iter().chain(record.delimiter))
        .collect()
}

fn test_crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0_u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320_u32 & 0_u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}

#[tokio::test]
async fn cancelled_start_releases_identity_after_owned_startup_cleanup() {
    let root = tempdir().unwrap();
    let input = root.path().join("input.log");
    fs::write(&input, b"line\n").unwrap();
    let manager = SourceManager::new(root.path().join("data"), small_config()).unwrap();
    let definition = file_source(SourceId::new(), &input, true);
    let mut start = Box::pin(manager.start(definition.clone()));
    assert!(matches!(
        start.as_mut().poll(&mut Context::from_waker(Waker::noop())),
        Poll::Pending
    ));
    drop(start);

    let retry = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match manager.start(definition.clone()).await {
                Err(RuntimeError::AlreadyRunning) => tokio::task::yield_now().await,
                result => return result,
            }
        }
    })
    .await
    .expect("cancelled startup cleanup timed out")
    .unwrap();
    retry.stop().await.unwrap();
}

#[tokio::test]
async fn shutdown_waits_for_a_start_already_in_progress() {
    let root = tempdir().unwrap();
    let input = root.path().join("input.log");
    fs::write(&input, b"line\n").unwrap();
    let manager = SourceManager::new(root.path().join("data"), small_config()).unwrap();
    let mut start = Box::pin(manager.start(file_source(SourceId::new(), &input, true)));
    assert!(matches!(
        start.as_mut().poll(&mut Context::from_waker(Waker::noop())),
        Poll::Pending
    ));
    assert!(manager.shutdown().await.is_empty());
    assert!(matches!(start.await, Err(RuntimeError::Closed)));
}

#[tokio::test]
async fn future_or_oversized_metadata_is_preserved_on_rejection() {
    for case in 0..3 {
        let root = tempdir().unwrap();
        let input = root.path().join("input.log");
        fs::write(&input, b"line\n").unwrap();
        let id = SourceId::new();
        let definition = file_source(id, &input, true);
        let original = if case == 2 {
            vec![b'x'; 1024 * 1024 + 1]
        } else {
            let mut persisted_definition = definition.clone();
            if case == 1 {
                persisted_definition.name = "different persisted definition".into();
            }
            serde_json::to_vec(&serde_json::json!({
                "schema_version": if case == 0 { 999 } else { 1 },
                "source_id": id,
                "generation": 5,
                "definition": persisted_definition,
            }))
            .unwrap()
        };
        let directory = root.path().join(id.0.to_string());
        fs::create_dir(&directory).unwrap();
        let metadata = directory.join("source.json");
        fs::write(&metadata, &original).unwrap();
        let manager = SourceManager::new(root.path(), small_config()).unwrap();
        assert!(manager.start(definition).await.is_err());
        assert_eq!(fs::read(metadata).unwrap(), original);
    }
}

#[tokio::test]
async fn torn_catalog_tail_is_separated_and_exposed_as_incomplete() {
    let root = tempdir().unwrap();
    let input = root.path().join("input.log");
    fs::write(&input, b"line\n").unwrap();
    let id = SourceId::new();
    let directory = root.path().join(id.0.to_string());
    fs::create_dir(&directory).unwrap();
    let catalog = directory.join("events.jsonl");
    fs::write(&catalog, b"{\"event\":\"stopped\"}\n{\"event\":\"torn").unwrap();
    let manager = SourceManager::new(root.path(), small_config()).unwrap();
    let handle = manager.start(file_source(id, &input, false)).await.unwrap();
    wait_for(&handle, |value| value.state == RuntimeState::Stopped).await;
    let bytes = fs::read(catalog).unwrap();
    assert!(bytes.ends_with(b"\n"));
    let events: Vec<serde_json::Value> = bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect();
    assert!(events.iter().any(|event| event["event"] == "incomplete"));
}

#[tokio::test]
async fn abort_deadline_covers_sustained_slow_writer_cleanup() {
    let root = tempdir().unwrap();
    let input = root.path().join("input.log");
    fs::write(&input, b"one\ntwo\nthree\n").unwrap();
    let mut config = small_config();
    config.batch_records = 1;
    config.writer_queue_capacity = 1;
    config.writer_delay = Duration::from_millis(300);
    config.graceful_stop_deadline = Duration::from_millis(40);
    let manager = SourceManager::new(root.path().join("data"), config).unwrap();
    let handle = manager
        .start(file_source(SourceId::new(), &input, false))
        .await
        .unwrap();
    wait_for(&handle, |progress| progress.records >= 1).await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    let report = tokio::time::timeout(Duration::from_millis(150), handle.abort())
        .await
        .expect("abort exceeded its caller deadline")
        .unwrap();
    assert!(!report.complete);
    wait_for(&handle, |progress| {
        progress.state == RuntimeState::Incomplete
    })
    .await;
}

#[tokio::test]
async fn small_throughput_baseline_stays_bounded_and_completes_promptly() {
    let root = tempdir().unwrap();
    let path = root.path().join("thousand.log");
    let fixture: Vec<u8> = (0..1000)
        .flat_map(|index| format!("event={index:04}\n").into_bytes())
        .collect();
    fs::write(&path, &fixture).unwrap();
    let started = Instant::now();
    let manager = SourceManager::new(root.path().join("capture"), small_config()).unwrap();
    let handle = manager
        .start(file_source(SourceId::new(), &path, false))
        .await
        .unwrap();
    wait_for(&handle, |value| value.state == RuntimeState::Stopped).await;
    assert!(handle.progress().records >= 1000);
    assert!(started.elapsed() < Duration::from_secs(5));
    let page = handle.read_page(0, usize::MAX, usize::MAX).await.unwrap();
    assert!(page.records.len() <= small_config().max_page_records);
    let captured: Vec<_> = all_records(&handle)
        .await
        .into_iter()
        .flat_map(|record| record.bytes.into_iter().chain(record.delimiter))
        .collect();
    assert_eq!(captured, fixture);
}
