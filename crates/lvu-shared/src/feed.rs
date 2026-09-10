//! Window-side progress feeder: poll canonical snapshots and publish them
//! into remote handles without ever stalling async or UI work.
//!
//! The split this module enforces (see the seam coordination notes):
//!
//! - The poll half (`WorkerClient::poll_progress`: socket roundtrip plus
//!   pure identity/session validation) performs no filesystem I/O and runs
//!   on the async runtime.
//! - The publish half (`RemoteSourceHandle::update_progress`) holds the
//!   handle's state mutex across bounded journal I/O, so each call runs on
//!   a bounded blocking lane (`spawn_blocking`), never on an executor or
//!   UI thread.
//! - UI and adapter workers read only the handle's separate publication
//!   slot (`progress()`), which never waits on the I/O mutex.
//!
//! One feeder task per handle keeps ticks serialized per source: concurrent
//! publishers cannot reorder latest-wins updates. Cancellation is task
//! abort — sleep and polls drop immediately; an in-flight blocking publish
//! runs to its bounded end. There is deliberately no shared feeder cache
//! here: the handle's own publication slot is the single authority, and a
//! second cache would race it.
//!
//! The worker session travels explicitly alongside the handle (never
//! re-derived inside): normally `client.worker_session()` sampled at the
//! same attach that constructed the handle. A replaced worker surfaces as
//! a poll error first (the client validates the session per answer), which
//! ends the loop so the caller re-attaches; a merely stale tick returns
//! `false` and the loop continues.

use std::{sync::Arc, time::Duration};

use tokio::sync::Mutex;

use crate::{RemoteSourceHandle, WorkerClient};

/// Default poll cadence: bounded staleness around one second while capture
/// is active. Stale is safe (delays tails, never wrong data); every answer
/// is sampled live, so staleness never exceeds the poll interval.
pub const DEFAULT_FEED_INTERVAL: Duration = Duration::from_secs(1);

/// Poll one snapshot and publish it into `handle`. Returns whether the
/// handle accepted it (a stale tick is refused with the cache untouched —
/// normal under racing ticks, not an error). `worker_session` must be the
/// session the handle was bound with; anything else refuses every publish.
/// Transport failures are errors: the worker is gone or confused, and the
/// caller re-attaches rather than caching.
pub async fn feed_once(
    client: &Arc<Mutex<WorkerClient>>,
    handle: &RemoteSourceHandle,
    worker_session: &str,
) -> Result<bool, String> {
    let source_id = handle.source_id();
    let progress = client.lock().await.poll_progress(source_id).await?;
    // The publish half holds the state mutex over journal I/O: blocking
    // lane, never the executor. Everything the closure touches is owned
    // (the handle is Clone, the snapshot moves, the session clones), so
    // no lock from this task crosses into the blocking thread.
    let fed = handle.clone();
    let session = worker_session.to_owned();
    tokio::task::spawn_blocking(move || fed.update_progress(&session, progress))
        .await
        .map_err(|error| format!("progress publish panicked: {error}"))
}

/// Serve one handle's feed loop until it fails or the task is aborted:
/// poll, publish, wait the interval, repeat. A fatal transport error ends
/// the loop with `Err` (caller re-attaches); stale snapshots just skip a
/// beat. Abort between iterations stops immediately; abort mid-publish
/// waits out only the bounded blocking call.
pub async fn serve_feed(
    client: Arc<Mutex<WorkerClient>>,
    handle: RemoteSourceHandle,
    worker_session: String,
    interval: Duration,
) -> Result<(), String> {
    loop {
        feed_once(&client, &handle, &worker_session).await?;
        tokio::time::sleep(interval).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FrameDecoder, RemoteConfig, WorkerEvent, encode_frame};
    use lvu_core::{ChunkPosition, RawRecord, RecordId, SourceId, StreamKind};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn peer_progress(
        source_id: SourceId,
        generation: u64,
        records: u64,
    ) -> lvu_ingest::SourceProgress {
        lvu_ingest::SourceProgress {
            source_id,
            generation,
            state: lvu_ingest::RuntimeState::Running,
            records,
            high_watermark: None,
            journal_bytes: 0,
            synced_records: 0,
            syncs: 0,
            handovers: 0,
            writer_cpu_nanos: 0,
            reader_cpu_nanos: 0,
            boundaries: 0,
            exit_code: None,
            discarded_bytes: 0,
            discarded_bytes_known: true,
            last_error: None,
        }
    }

    fn build_journal(
        dir: &std::path::Path,
        source_id: SourceId,
        bodies: &[&str],
    ) -> std::path::PathBuf {
        let path = dir.join("capture.journal");
        let (mut journal, _) =
            lvu_core::journal::Journal::open(&path, source_id).expect("open journal");
        for (sequence, body) in bodies.iter().enumerate() {
            journal
                .append(RawRecord {
                    record_id: RecordId {
                        source_id,
                        sequence: sequence as u64,
                    },
                    captured_at_unix_nanos: 0,
                    stream: StreamKind::File,
                    bytes: body.as_bytes().to_vec().into(),
                    delimiter: b"\n".to_vec().into(),
                    acquisition_id: uuid::Uuid::new_v4(),
                    chunk: ChunkPosition::Complete,
                })
                .expect("append");
        }
        journal.flush().expect("flush");
        path
    }

    /// Scripted peer: Welcome once, then one progress answer per request
    /// from the script. Mirrors the client.rs harness.
    async fn scripted_peer(listener: tokio::net::UnixListener, replies: Vec<WorkerEvent>) {
        let (stream, _) = listener.accept().await.expect("peer accepts");
        let (reader, mut writer) = stream.into_split();
        let mut reader = tokio::io::BufReader::new(reader);
        let mut decoder = FrameDecoder::new();
        let mut buffer = vec![0u8; crate::READ_CHUNK_BYTES];
        let mut replies = replies.into_iter();
        loop {
            let count = reader.read(&mut buffer).await.expect("peer reads");
            if count == 0 {
                return;
            }
            let values = decoder.push_bytes(&buffer[..count]).expect("peer decodes");
            for _value in values {
                let Some(reply) = replies.next() else {
                    return;
                };
                let wire = encode_frame(&serde_json::to_value(&reply).expect("peer encodes"))
                    .expect("peer frames");
                writer.write_all(&wire).await.expect("peer writes");
            }
        }
    }

    fn progress_reply(
        request_id: &str,
        session: &str,
        progress: lvu_ingest::SourceProgress,
    ) -> WorkerEvent {
        WorkerEvent::SourceProgress {
            request_id: request_id.into(),
            worker_session: session.into(),
            progress,
        }
    }

    #[tokio::test]
    async fn feed_once_publishes_and_stale_tick_skips() {
        let root = tempfile::tempdir().unwrap();
        crate::election::WorkerPaths::new(root.path())
            .ensure_directories()
            .expect("viewer directories");
        let socket = root.path().join("control.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let source_id = SourceId(uuid::Uuid::from_u128(801));
        let journal = build_journal(root.path(), source_id, &["a", "b", "c"]);
        let welcome = WorkerEvent::Welcome {
            request_id: "h".into(),
            worker_pid: 1,
            protocol: crate::protocol::PROTOCOL_VERSION,
            worker_session: "session-feed".into(),
            sources: Vec::new(),
        };
        // Initial poll, fresh tick, then a regressed generation: accepted,
        // accepted, refused-with-cache-untouched.
        let script = vec![
            welcome,
            progress_reply("p1", "session-feed", peer_progress(source_id, 1, 3)),
            progress_reply("p2", "session-feed", peer_progress(source_id, 1, 5)),
            progress_reply("p3", "session-feed", peer_progress(source_id, 0, 99)),
        ];
        tokio::spawn(scripted_peer(listener, script));
        let (client, _) = WorkerClient::connect(root.path(), &socket, "window-f", 5201)
            .await
            .expect("connect");
        let client = Arc::new(Mutex::new(client));
        // Construction takes the first poll as the trusted initial cache.
        let initial = client
            .lock()
            .await
            .poll_progress(source_id)
            .await
            .expect("initial poll");
        let handle = RemoteSourceHandle::new(
            source_id,
            &journal,
            "session-feed".into(),
            initial,
            RemoteConfig::default(),
        )
        .expect("register");
        assert_eq!(handle.progress().records, 3);
        // Fresh tick publishes through the blocking lane.
        assert!(
            feed_once(&client, &handle, "session-feed")
                .await
                .expect("feed"),
            "fresh tick accepted"
        );
        assert_eq!(handle.progress().records, 5);
        // Regressed generation refuses with the cache untouched.
        assert!(
            !feed_once(&client, &handle, "session-feed")
                .await
                .expect("feed"),
            "regressed tick refused"
        );
        assert_eq!(handle.progress().records, 5);
        assert_eq!(handle.progress().generation, 1);
    }

    #[test]
    fn feed_interval_is_one_second() {
        assert_eq!(DEFAULT_FEED_INTERVAL, Duration::from_secs(1));
    }
}
