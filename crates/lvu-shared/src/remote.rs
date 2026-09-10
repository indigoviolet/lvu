//! Read-only remote capture handles for foreground windows.
//!
//! A window attached to a worker never owns capture: the worker's
//! `SourceManager`, writer tasks, leases, cursors and commits stay in the
//! worker process, and lifecycle (start/stop/restart) travels only as RPCs.
//! What a window needs to drive its live and query adapters is three things
//! the worker already publishes elsewhere: journal bytes on a shared
//! filesystem, canonical progress snapshots, and a stable generation fence.
//! This module binds those three into the same read seam the local adapters
//! already speak, without duplicating any read model:
//!
//! * bytes come from [`FileJournalTail`] over the shared journal path —
//!   never across the control channel, never through a second decoder;
//! * progress is the canonical `lvu_ingest::SourceProgress`, cached latest
//!   wins from the status stream and polled exactly like a local watch;
//! * fencing is the snapshot's `(worker_session, generation)` pair plus the
//!   tail-continuity check below, which re-anchors instead of rescanning.
//!
//! Read-only is enforced by absence: this module offers no stop, abort,
//! restart, history mutation or cursor movement of any kind. Union inputs
//! stay local-only in phase 1 ([`AnySourceHandle::as_local`] is `None` for
//! remote inputs, refused explicitly at the call site) until worker-attested
//! cross-process fencing exists; no remote guard ever poses as worker
//! publication authority.

use std::{
    io,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use lvu_core::{SourceId, journal::JournalPage};
use lvu_ingest::{RuntimeError, SourceHandle, SourceProgress};
use tokio::sync::Semaphore;

use crate::tail::{Continuity, FileIdentity, FileJournalTail, TailStatus, classify};

/// Bounds for one remote page read, mirroring the ingest clamps so a remote
/// window can never ask for more than a local adapter could.
#[derive(Clone, Debug)]
pub struct RemoteConfig {
    pub max_page_records: usize,
    pub max_page_bytes: usize,
}

impl Default for RemoteConfig {
    fn default() -> Self {
        Self {
            max_page_records: 8192,
            max_page_bytes: 4 * 1024 * 1024,
        }
    }
}

#[derive(Debug)]
struct RemoteInner {
    source_id: SourceId,
    tail: FileJournalTail,
    /// Worker lifetime this handle is bound to (from `Welcome`/attach).
    /// Snapshots naming another session are dropped: same generation under
    /// a new session must re-register, never alias a different capture.
    worker_session: String,
    /// Latest accepted canonical snapshot. Always populated: construction
    /// takes the snapshot the app registered from, so `progress()` is
    /// infallible exactly like the local watch it mirrors.
    progress: Mutex<SourceProgress>,
    /// Filesystem state at the last successful read or tick poll, for the
    /// cheap per-read change detection below.
    file_state: Mutex<Option<(FileIdentity, u64)>>,
    /// Last tail status, for continuity re-anchoring on progress ticks.
    last_status: Mutex<Option<TailStatus>>,
    /// One outstanding page per source, mirroring the ingest page gate so
    /// the bound live indexing depends on survives without a writer hop.
    gate: Arc<Semaphore>,
    config: RemoteConfig,
    /// Replacements observed and re-anchored. Diagnostic only.
    replacements: AtomicU64,
}

/// A read-only view of one worker-owned capture, speaking the live/query
/// read seam (`source_id` / `progress` / `read_page`) over a shared journal
/// path plus canonical progress snapshots.
#[derive(Clone, Debug)]
pub struct RemoteSourceHandle {
    inner: Arc<RemoteInner>,
}

fn changed_error() -> RuntimeError {
    RuntimeError::Io(io::Error::other(
        "journal file changed under read; re-anchor via progress fences",
    ))
}

impl RemoteSourceHandle {
    /// Bind to one worker lifetime. `initial` must be the snapshot the app
    /// registered from (same status message); the cache is never empty
    /// after, so `progress()` cannot fail.
    pub fn new(
        source_id: SourceId,
        journal_path: &Path,
        worker_session: String,
        initial: SourceProgress,
        config: RemoteConfig,
    ) -> Self {
        Self {
            inner: Arc::new(RemoteInner {
                source_id,
                tail: FileJournalTail::new(source_id, journal_path),
                worker_session,
                progress: Mutex::new(initial),
                file_state: None.into(),
                last_status: None.into(),
                gate: Arc::new(Semaphore::new(1)),
                config,
                replacements: AtomicU64::new(0),
            }),
        }
    }

    pub fn source_id(&self) -> SourceId {
        self.inner.source_id
    }

    /// Latest accepted canonical snapshot. Mirrors the local watch read:
    /// always current, never torn, never synthesized.
    pub fn progress(&self) -> SourceProgress {
        self.inner
            .progress
            .lock()
            .expect("remote progress poisoned")
            .clone()
    }

    /// Feed one status-stream snapshot. Returns false without touching
    /// anything when the snapshot names another worker session: same
    /// generation under a new session must re-register through a fresh
    /// handle, never alias a different capture. Otherwise polls the tail,
    /// re-anchors continuity, and publishes the snapshot. Synchronous and
    /// bounded (one metadata read plus one bounded anchor page); feeders
    /// call it per progress tick.
    pub fn update_progress(&self, worker_session: &str, snapshot: SourceProgress) -> bool {
        if self.inner.worker_session != worker_session {
            return false;
        }
        // Poll first, lock nothing across I/O: the locks below are only ever
        // taken to store already-observed facts.
        let fresh = match self.inner.tail.poll_status() {
            Ok(status) => status,
            Err(_) => {
                // An unreadable journal keeps the last good snapshot rather
                // than publishing absence: readers fail closed on the file
                // itself, and the next tick retries.
                self.store_snapshot(snapshot);
                return true;
            }
        };
        let previous = self
            .inner
            .last_status
            .lock()
            .expect("remote tail status poisoned")
            .clone();
        if let Some(previous) = previous.as_ref()
            && classify(previous, &fresh) == Continuity::Replaced
        {
            self.inner.replacements.fetch_add(1, Ordering::Relaxed);
        }
        *self
            .inner
            .last_status
            .lock()
            .expect("remote tail status poisoned") = Some(fresh.clone());
        *self
            .inner
            .file_state
            .lock()
            .expect("remote file state poisoned") = Some((fresh.identity.clone(), fresh.file_len));
        self.store_snapshot(snapshot);
        true
    }

    fn store_snapshot(&self, snapshot: SourceProgress) {
        *self
            .inner
            .progress
            .lock()
            .expect("remote progress poisoned") = snapshot;
    }

    /// Read one bounded page through the shared journal file. Gate-serialized
    /// to one outstanding page per source, like the local path. A filesystem
    /// change observed before the read, or across it, fails closed instead
    /// of serving cross-file rows: callers treat the error as failed/pending
    /// and re-anchor through the progress and epoch fences. The gate permit
    /// moves into the blocking closure through service or drop, so
    /// cancelling mid-read still leaves exactly one active read per source.
    pub async fn read_page(
        &self,
        offset: u64,
        max_records: usize,
        max_bytes: usize,
    ) -> Result<JournalPage, RuntimeError> {
        let bounded_records = max_records.min(self.inner.config.max_page_records);
        let bounded_bytes = max_bytes.min(self.inner.config.max_page_bytes);
        let path = self.inner.tail.journal_path().to_owned();
        // Cheap change detection before spending a gate slot: metadata only.
        let (pre_identity, pre_len) = FileIdentity::of(&path).map_err(RuntimeError::Io)?;
        let remembered = self
            .inner
            .file_state
            .lock()
            .expect("remote file state poisoned")
            .clone();
        if let Some((identity, len)) = remembered
            && (pre_identity != identity || pre_len < len)
        {
            return Err(changed_error());
        }
        let permit = self
            .inner
            .gate
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| RuntimeError::Closed)?;
        let tail = self.inner.tail.clone();
        let page = tokio::task::spawn_blocking(move || {
            let _guard = permit;
            tail.read_page(offset, bounded_records, bounded_bytes)
                .map_err(RuntimeError::from)
        })
        .await
        .map_err(RuntimeError::from)??;
        // Fence the read just served: rows raced by a replacement mid-read
        // are discarded instead of delivered.
        let (post_identity, post_len) = FileIdentity::of(&path).map_err(RuntimeError::Io)?;
        if post_identity != pre_identity || post_len < pre_len {
            return Err(changed_error());
        }
        *self
            .inner
            .file_state
            .lock()
            .expect("remote file state poisoned") = Some((post_identity, post_len));
        Ok(page)
    }

    /// Replacements observed and re-anchored. Diagnostic only.
    pub fn replacements_observed(&self) -> u64 {
        self.inner.replacements.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    //! The seam is new; every verdict below is order/content, never timing.
    //! Slow-reader races are resolved by construction (impossible interleaving
    //! asserted nowhere): cancellation stimulation is best-effort while all
    //! assertions hold under every interleaving.
    use super::*;
    use lvu_core::{ChunkPosition, RawRecord, RecordId, StreamKind};
    use lvu_ingest::RuntimeState;

    fn test_record(source_id: SourceId, sequence: u64, body: &str) -> RawRecord {
        RawRecord {
            record_id: RecordId {
                source_id,
                sequence,
            },
            captured_at_unix_nanos: 0,
            stream: StreamKind::File,
            bytes: body.as_bytes().to_vec().into(),
            delimiter: b"\n".to_vec().into(),
            acquisition_id: uuid::Uuid::new_v4(),
            chunk: ChunkPosition::Complete,
        }
    }

    fn snapshot(source_id: SourceId, generation: u64, records: u64) -> SourceProgress {
        SourceProgress {
            source_id,
            generation,
            state: RuntimeState::Running,
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
                .append(test_record(source_id, sequence as u64, body))
                .expect("append");
        }
        journal.flush().expect("flush");
        path
    }

    #[test]
    fn handles_are_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<RemoteSourceHandle>();
        assert_send_sync::<AnySourceHandle>();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn remote_reads_rows_and_tracks_snapshots() {
        let dir = tempfile::tempdir().unwrap();
        let source_id = SourceId(uuid::Uuid::from_u128(101));
        let journal_path = build_journal(dir.path(), source_id, &["a", "b", "c"]);
        let handle = RemoteSourceHandle::new(
            source_id,
            &journal_path,
            "session-a".into(),
            snapshot(source_id, 1, 3),
            RemoteConfig::default(),
        );
        let page = handle
            .read_page(0, 128, 1024 * 1024)
            .await
            .expect("read rows");
        assert_eq!(page.records.len(), 3);
        assert_eq!(page.records[0].bytes.as_slice(), b"a");
        assert_eq!(page.records[2].record_id.sequence, 2);
        assert_eq!(handle.progress().generation, 1);
        // Same-session update applies; foreign session is dropped with the
        // cache untouched, so a new worker lifetime can never alias in.
        assert!(handle.update_progress("session-a", snapshot(source_id, 1, 5)));
        assert_eq!(handle.progress().records, 5);
        assert!(!handle.update_progress("session-b", snapshot(source_id, 9, 99)));
        assert_eq!(handle.progress().generation, 1);
        assert_eq!(handle.progress().records, 5);
        assert_eq!(handle.replacements_observed(), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn replacement_reanchors_and_serves_new_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let source_id = SourceId(uuid::Uuid::from_u128(102));
        let journal_path = build_journal(dir.path(), source_id, &["old"]);
        let handle = RemoteSourceHandle::new(
            source_id,
            &journal_path,
            "session-a".into(),
            snapshot(source_id, 1, 1),
            RemoteConfig::default(),
        );
        // Replace out from under the handle with different, longer content,
        // then tick: the poll sees the new file against the remembered one.
        // Every filesystem outcome here (new inode, same inode) classifies
        // Replaced — same inode still differs in length and anchor — so the
        // verdict does not depend on allocation luck.
        assert!(handle.update_progress("session-a", snapshot(source_id, 1, 1)));
        assert_eq!(handle.replacements_observed(), 0);
        std::fs::remove_file(&journal_path).unwrap();
        build_journal(dir.path(), source_id, &["new-content-longer"]);
        assert!(handle.update_progress("session-a", snapshot(source_id, 1, 1)));
        assert_eq!(handle.replacements_observed(), 1);
        let page = handle
            .read_page(0, 128, 1024 * 1024)
            .await
            .expect("re-anchored read");
        assert_eq!(page.records[0].bytes.as_slice(), b"new-content-longer");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn shrink_fails_reads_closed() {
        let dir = tempfile::tempdir().unwrap();
        let source_id = SourceId(uuid::Uuid::from_u128(103));
        let journal_path = build_journal(dir.path(), source_id, &["a", "b", "c"]);
        let handle = RemoteSourceHandle::new(
            source_id,
            &journal_path,
            "session-a".into(),
            snapshot(source_id, 1, 3),
            RemoteConfig::default(),
        );
        // Prime the remembered state, then truncate in place (same inode,
        // shorter file): the next read must fail closed, never serve a
        // truncated prefix as whole.
        assert!(handle.update_progress("session-a", snapshot(source_id, 1, 3)));
        std::fs::File::create(&journal_path)
            .unwrap()
            .set_len(16)
            .unwrap();
        let error = handle
            .read_page(0, 128, 1024 * 1024)
            .await
            .expect_err("shrunk journal must fail closed");
        // Fail-closed as changed-under-read (an I/O race diagnostic), never
        // a truncated prefix served as whole: either error shape proves the
        // bound, and this one names the cause.
        assert!(
            matches!(error, RuntimeError::Io(_)),
            "unexpected error shape: {error:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn cancelled_reads_do_not_break_later_reads() {
        let dir = tempfile::tempdir().unwrap();
        let source_id = SourceId(uuid::Uuid::from_u128(104));
        let mut bodies = Vec::new();
        for index in 0..50 {
            bodies.push(format!("row-{index:02}"));
        }
        let refs: Vec<&str> = bodies.iter().map(String::as_str).collect();
        let journal_path = build_journal(dir.path(), source_id, &refs);
        let handle = RemoteSourceHandle::new(
            source_id,
            &journal_path,
            "session-a".into(),
            snapshot(source_id, 1, 50),
            RemoteConfig::default(),
        );
        // Race cancellations against in-flight reads. Verdicts hold under
        // every interleaving: an abort before admission sends nothing, an
        // abort mid-read detaches with the guard held, and the permit is
        // always released exactly once by service or drop.
        for _ in 0..20 {
            let racing = handle.clone();
            let task = tokio::spawn(async move { racing.read_page(0, 256, 4 * 1024 * 1024).await });
            tokio::task::yield_now().await;
            task.abort();
        }
        let page = handle
            .read_page(0, 256, 4 * 1024 * 1024)
            .await
            .expect("read after cancellations");
        assert_eq!(page.records.len(), 50);
        assert_eq!(page.records[0].bytes.as_slice(), b"row-00");
        assert_eq!(page.records[49].bytes.as_slice(), b"row-49");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn any_handle_delegates_and_gates_union() {
        let dir = tempfile::tempdir().unwrap();
        let source_id = SourceId(uuid::Uuid::from_u128(105));
        let journal_path = build_journal(dir.path(), source_id, &["x", "y"]);
        let remote = RemoteSourceHandle::new(
            source_id,
            &journal_path,
            "session-a".into(),
            snapshot(source_id, 2, 2),
            RemoteConfig::default(),
        );
        let any = AnySourceHandle::Remote(remote);
        assert_eq!(any.source_id(), source_id);
        assert_eq!(any.progress().generation, 2);
        let page = any.read_page(0, 128, 1024 * 1024).await.expect("read");
        assert_eq!(page.records.len(), 2);
        // Union phase gate: remote inputs expose no local handle, so union
        // publication cannot mistake them for fenced local inputs.
        assert!(any.as_local().is_none());
    }
}

/// Local-or-remote source input for live and query adapters. Method names
/// and signatures match the traced read seam exactly so call bodies barely
/// change; union paths take `as_local` instead, which refuses remote inputs
/// explicitly until attested cross-process fencing exists.
/// Local-or-remote source input for live and query adapters. Method names
/// and signatures match the traced read seam exactly so call bodies barely
/// change. Clone (not Debug: `SourceHandle` itself is not `Debug`) lets
/// workers clone inputs into tasks exactly as today.
#[derive(Clone)]
pub enum AnySourceHandle {
    Local(SourceHandle),
    Remote(RemoteSourceHandle),
}

impl AnySourceHandle {
    pub fn source_id(&self) -> SourceId {
        match self {
            Self::Local(handle) => handle.source_id(),
            Self::Remote(handle) => handle.source_id(),
        }
    }

    pub fn progress(&self) -> SourceProgress {
        match self {
            Self::Local(handle) => handle.progress(),
            Self::Remote(handle) => handle.progress(),
        }
    }

    pub async fn read_page(
        &self,
        offset: u64,
        max_records: usize,
        max_bytes: usize,
    ) -> Result<JournalPage, RuntimeError> {
        match self {
            Self::Local(handle) => handle.read_page(offset, max_records, max_bytes).await,
            Self::Remote(handle) => handle.read_page(offset, max_records, max_bytes).await,
        }
    }

    /// Union-only phase gate: the local handle for generation-fenced union
    /// publication, or `None` for remote inputs, which the call site refuses
    /// with an actionable error until attested cross-process fencing exists.
    /// Offered nowhere else: all other paths go through the read seam above.
    pub fn as_local(&self) -> Option<&SourceHandle> {
        match self {
            Self::Local(handle) => Some(handle),
            Self::Remote(_) => None,
        }
    }
}
