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
        Arc, Mutex, RwLock,
        atomic::{AtomicU64, Ordering},
    },
};

use lvu_core::{SourceId, journal::JournalPage};
use lvu_ingest::{RuntimeError, SourceHandle, SourceProgress};
use tokio::sync::Semaphore;

use crate::tail::{Continuity, FileJournalTail, TailStatus, classify};

/// Bounds for one remote page read, mirroring the ingest clamps so a remote
/// window can never ask for more than a local adapter could. Fields are
/// private and the constructor validates: a `usize::MAX` ceiling would
/// silently unbound every read, and a zero bound would serve nothing.
#[derive(Clone, Debug)]
pub struct RemoteConfig {
    max_page_records: usize,
    max_page_bytes: usize,
}

/// Immutable ceilings for remote page reads, matching the ingest clamps.
pub const MAX_PAGE_RECORDS: usize = 8192;
/// Immutable ceilings for remote page reads, matching the ingest clamps.
pub const MAX_PAGE_BYTES: usize = 4 * 1024 * 1024;

impl RemoteConfig {
    /// Validated bounds: both must be nonzero (a zero bound serves nothing)
    /// and within the immutable ceilings above.
    pub fn new(max_page_records: usize, max_page_bytes: usize) -> Result<Self, String> {
        if max_page_records == 0 || max_page_records > MAX_PAGE_RECORDS {
            return Err(format!(
                "max_page_records {max_page_records} outside 1..={MAX_PAGE_RECORDS}"
            ));
        }
        if max_page_bytes == 0 || max_page_bytes > MAX_PAGE_BYTES {
            return Err(format!(
                "max_page_bytes {max_page_bytes} outside 1..={MAX_PAGE_BYTES}"
            ));
        }
        Ok(Self {
            max_page_records,
            max_page_bytes,
        })
    }
}

impl Default for RemoteConfig {
    fn default() -> Self {
        Self {
            max_page_records: MAX_PAGE_RECORDS,
            max_page_bytes: MAX_PAGE_BYTES,
        }
    }
}

#[derive(Debug)]
struct RemoteState {
    /// Latest accepted canonical snapshot. Always populated.
    progress: SourceProgress,
    /// Last tail observation, for continuity re-anchoring on ticks.
    last_status: Option<TailStatus>,
    /// Filesystem plus content-anchor state at the last observation, for
    /// per-read change detection. Identity and length alone cannot see a
    /// same-inode equal-length in-place replacement; the anchor (first
    /// record acquisition plus length) can, so this stores the full
    /// `TailStatus` and reads compare via `classify`. Written by updates,
    /// and by the first read when no tick has run yet.
    file_state: Option<TailStatus>,
    /// Generation at which continuity last failed, if any. Reads stay
    /// failed until a snapshot with a strictly newer generation arrives —
    /// the newer worker-generation authority that ends the distrust — so
    /// old-generation offsets never return replacement bytes. A restored
    /// file without a restart does not clear this: tampering once observed
    /// is distrusted until authority moves.
    invalid_generation: Option<u64>,
}

#[derive(Debug)]
struct RemoteInner {
    source_id: SourceId,
    tail: FileJournalTail,
    /// Worker lifetime this handle is bound to (from `Welcome`/attach).
    /// Snapshots naming another session are dropped: same generation under
    /// a new session must re-register, never alias a different capture.
    worker_session: String,
    /// Identity, validation cache and continuity state in one mutex: every
    /// update validates, polls, classifies and publishes atomically, so a
    /// delayed older update always validates against the newest cache and
    /// can never overwrite it. The guard is held across the bounded
    /// synchronous poll (one metadata read plus one bounded anchor page,
    /// never an await); nothing else is ever taken under it except the
    /// brief publication write below (no IO under that lock), so no lock
    /// order exists to invert.
    state: Mutex<RemoteState>,
    /// Canonical publication slot for [`RemoteSourceHandle::progress`].
    /// Written only under the state guard on accepted updates (one brief
    /// clone after the poll, never over IO) and read without the state
    /// guard, so UI getters never block on the IO mutex and never see a
    /// torn or out-of-order snapshot. The feeder (5b-owned) publishes
    /// through `update_progress`; this slot only mirrors accepts.
    published: RwLock<SourceProgress>,
    /// Authority epoch, bumped whenever invalidation is set or cleared.
    /// Reads sample it beside their checks and discard when it moved
    /// mid-read, which is what makes the check-then-read race fail closed
    /// without serializing reads against ticks.
    authority: AtomicU64,
    /// One outstanding page per source, mirroring the ingest page gate so
    /// the bound live indexing depends on survives without a writer hop.
    gate: Arc<Semaphore>,
    config: RemoteConfig,
    /// Replacements observed and re-anchored. Diagnostic only, never fed
    /// into generations or fences.
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
    /// registered from (same status message), and it must already name this
    /// source: a foreign initial snapshot fails construction rather than
    /// poisoning the cache the first read would trust.
    pub fn new(
        source_id: SourceId,
        journal_path: &Path,
        worker_session: String,
        initial: SourceProgress,
        config: RemoteConfig,
    ) -> Result<Self, String> {
        if initial.source_id != source_id {
            return Err("initial snapshot names another source".into());
        }
        Ok(Self {
            inner: Arc::new(RemoteInner {
                source_id,
                tail: FileJournalTail::new(source_id, journal_path),
                worker_session,
                state: Mutex::new(RemoteState {
                    progress: initial.clone(),
                    last_status: None,
                    file_state: None,
                    invalid_generation: None,
                }),
                published: RwLock::new(initial),
                authority: AtomicU64::new(0),
                gate: Arc::new(Semaphore::new(1)),
                config,
                replacements: AtomicU64::new(0),
            }),
        })
    }

    pub fn source_id(&self) -> SourceId {
        self.inner.source_id
    }

    /// The worker lifetime this handle is bound to, for stamping requests
    /// and detecting replacement without touching capture state.
    /// Coordinated accessor (5b-owned seam addition): feeders and the
    /// union transport read it; updates still go only through
    /// `update_progress`.
    pub fn worker_session(&self) -> &str {
        &self.inner.worker_session
    }

    /// Latest accepted canonical snapshot. Mirrors the local watch read:
    /// always current, never torn, never synthesized. Reads the publication
    /// slot only, never the IO-guarded state mutex, so UI getters never
    /// block on a poll.
    pub fn progress(&self) -> SourceProgress {
        self.inner
            .published
            .read()
            .expect("remote progress poisoned")
            .clone()
    }

    /// Feed one status-stream snapshot. Returns false without touching
    /// anything when the snapshot is stale or foreign, preserving the last
    /// good cache: another source's snapshot, an older generation, or
    /// records/high-watermark moving backward inside one generation are all
    /// refused. A newer generation is always accepted — crash recovery can
    /// legitimately truncate torn tail bytes, and the generation bump itself
    /// is what re-registers downstream consumers. Poll errors likewise
    /// refuse without storing: an unreadable journal tells nothing, so the
    /// previous snapshot stands instead of a gap masquerading as data.
    ///
    /// Admission, poll, classify and publish are one serialized transaction
    /// under the state lock (held across the bounded synchronous poll, never
    /// an await): a delayed older update always validates against the newest
    /// cache and can never overwrite it.
    pub fn update_progress(&self, worker_session: &str, snapshot: SourceProgress) -> bool {
        if self.inner.worker_session != worker_session {
            return false;
        }
        if snapshot.source_id != self.inner.source_id {
            return false;
        }
        let mut state = self.inner.state.lock().expect("remote state poisoned");
        if snapshot.generation < state.progress.generation {
            return false;
        }
        if snapshot.generation == state.progress.generation {
            if snapshot.records < state.progress.records {
                return false;
            }
            if Self::hw_sequence(&snapshot.high_watermark)
                < Self::hw_sequence(&state.progress.high_watermark)
            {
                return false;
            }
        }
        let fresh = match self.inner.tail.poll_status() {
            Ok(status) => status,
            Err(_) => return false,
        };
        // Previous observation for replacement detection: the last tick
        // when one exists, otherwise the read-established state. Without
        // the fallback a replacement landing between the first pre-tick
        // read and the first tick would be silently adopted.
        let previous = state.last_status.as_ref().or(state.file_state.as_ref());
        let mut bumped = false;
        if matches!(previous, Some(prev) if classify(prev, &fresh) == Continuity::Replaced) {
            self.inner.replacements.fetch_add(1, Ordering::Relaxed);
            // Distrust, but only without newer authority: when this
            // snapshot's generation is already ahead of the cached one,
            // it IS the new authority describing the restarted capture,
            // so it adopts immediately instead of waiting one more
            // generation. Same-generation replacement keeps reads failed
            // until authority genuinely moves.
            if snapshot.generation == state.progress.generation {
                state.invalid_generation = Some(snapshot.generation);
                self.inner.authority.fetch_add(1, Ordering::Release);
                bumped = true;
            } else if snapshot.generation > state.progress.generation {
                // Newer-generation adoption is still an authority move:
                // reads admitted before adoption sampled the old epoch
                // and must discard, while reads starting after adoption
                // sample the new epoch and serve the new bytes.
                self.inner.authority.fetch_add(1, Ordering::Release);
                bumped = true;
            }
        }
        state.last_status = Some(fresh.clone());
        state.file_state = Some(fresh.clone());
        // A newer generation ends the distrust above: the worker moved
        // authority forward, and downstream re-registers on the bump. The
        // clear is independent of whether this same tick already bumped for
        // newer-generation adoption: leaving `Some(1)` behind after adopting
        // gen-2 would fail the immediate gen-2 read without a third tick.
        // One tick still moves the epoch exactly once.
        if matches!(state.invalid_generation, Some(held) if snapshot.generation > held) {
            state.invalid_generation = None;
            if !bumped {
                self.inner.authority.fetch_add(1, Ordering::Release);
            }
        }
        // Publish only on accept, under the same state guard so a delayed
        // older update can never overwrite a newer publication: validation
        // above already refused it. The publication write itself is one
        // brief clone after the poll, never over IO, while readers take only
        // the publication lock.
        state.progress = snapshot.clone();
        *self
            .inner
            .published
            .write()
            .expect("remote progress poisoned") = snapshot;
        true
    }

    /// High-watermark sequence, with absence ordered below everything: a
    /// watermark appears once records exist and never retreats within one
    /// generation.
    fn hw_sequence(high_watermark: &Option<lvu_core::RecordId>) -> Option<u64> {
        high_watermark.map(|id| id.sequence)
    }

    /// Read one bounded page through the shared journal file. Gate-serialized
    /// to one outstanding page per source, like the local path. Reads stay
    /// failed while continuity is distrusted: only a strictly newer
    /// generation re-authorizes them, so old-generation offsets never return
    /// replacement bytes. A content change observed before the read, or
    /// across it, likewise fails closed instead of serving cross-file rows:
    /// identity and length alone cannot see a same-inode equal-length
    /// in-place replacement, so both fences compare the full tail status
    /// (identity, length, content anchor) via `classify`. Callers treat the
    /// error as failed/pending and re-anchor through the progress and epoch
    /// fences. An authority sample taken beside the entry checks is compared
    /// after service, which is what makes an invalidation landing mid-read
    /// discard instead of deliver. The gate permit moves into the blocking
    /// closure through service or drop, so cancelling mid-read still leaves
    /// exactly one active read per source.
    pub async fn read_page(
        &self,
        offset: u64,
        max_records: usize,
        max_bytes: usize,
    ) -> Result<JournalPage, RuntimeError> {
        let authority = &self.inner.authority;
        let observed = authority.load(Ordering::Acquire);
        {
            let state = self.inner.state.lock().expect("remote state poisoned");
            if state.invalid_generation.is_some() {
                return Err(changed_error());
            }
        }
        let bounded_records = max_records.min(self.inner.config.max_page_records);
        let bounded_bytes = max_bytes.min(self.inner.config.max_page_bytes);
        // Anchor-evidence change detection before spending a gate slot: a
        // full poll (identity, length, content anchor), compared via
        // `classify` so same-inode equal-length in-place replacement fails.
        // The first read also establishes the remembered state when no tick
        // has run yet, under the same state transaction: without this, reads
        // before the first tick would have nothing to compare against and a
        // replacement in that window would serve new bytes under no
        // authority at all.
        let pre = self.inner.tail.poll_status().map_err(RuntimeError::Io)?;
        {
            let mut state = self.inner.state.lock().expect("remote state poisoned");
            match state.file_state.clone() {
                Some(remembered) => {
                    if classify(&remembered, &pre) == Continuity::Replaced {
                        return Err(changed_error());
                    }
                }
                None => {
                    state.file_state = Some(pre.clone());
                }
            }
        }
        let permit = self
            .inner
            .gate
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| RuntimeError::Closed)?;
        // The invalidation set may have changed while acquiring: re-check
        // under no lock beyond this statement (the permit drops correctly on
        // this early return).
        if self
            .inner
            .state
            .lock()
            .expect("remote state poisoned")
            .invalid_generation
            .is_some()
        {
            return Err(changed_error());
        }
        #[cfg(test)]
        read_probe::rendezvous(&self.inner.source_id);
        let tail = self.inner.tail.clone();
        let page = tokio::task::spawn_blocking(move || {
            let _guard = permit;
            tail.read_page(offset, bounded_records, bounded_bytes)
                .map_err(RuntimeError::from)
        })
        .await
        .map_err(RuntimeError::from)??;
        // Fence the read just served: rows raced by a replacement mid-read
        // are discarded instead of delivered. Anchor-evidence again, so a
        // same-inode equal-length replacement across service still fails
        // even though identity and length did not move.
        let post = self.inner.tail.poll_status().map_err(RuntimeError::Io)?;
        if classify(&pre, &post) == Continuity::Replaced {
            return Err(changed_error());
        }
        // Post-poll rendezvous (test-only): `pre`/`post` are both taken, so
        // a replacement landing here passes anchor checks by construction
        // and only the authority epoch below can fail the read. This is what
        // isolates the epoch from the anchor fence.
        #[cfg(test)]
        read_probe::rendezvous_post(&self.inner.source_id);
        if authority.load(Ordering::Acquire) != observed {
            return Err(changed_error());
        }
        Ok(page)
    }

    /// Replacements observed and re-anchored. Diagnostic only, never fed
    /// into generations or fences.
    pub fn replacements_observed(&self) -> u64 {
        self.inner.replacements.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
pub(crate) mod read_probe {
    //! Test-only rendezvous inside [`super::RemoteSourceHandle::read_page`],
    //! parked after admission and before service, plus a second rendezvous
    //! after the post-service poll and before the authority check.
    //! Production builds compile it out entirely; unregistered sources
    //! proceed immediately. Each registry is keyed by source, and every test
    //! uses a fresh source identity, so parallel tests never meet. A parked
    //! task holds no locks, only its gate permit (bounded by the test that
    //! armed it); dropping the release sender unblocks it, so a panicking
    //! test cannot leak a parked thread.
    //!
    //! The pre-service rendezvous proves end-to-end discard but does not
    //! isolate the epoch: with a same-length replacement the post anchor
    //! already rejects. The post-poll rendezvous parks after `pre`/`post`
    //! are both taken (both old, classify passes), so only the authority
    //! counter can fail the released read.
    use lvu_core::SourceId;
    use std::collections::HashMap;
    use std::sync::{Arc, Barrier, Mutex, OnceLock};

    struct Probe {
        entered: Arc<Barrier>,
        release: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    }

    fn registry() -> &'static Mutex<HashMap<SourceId, Arc<Probe>>> {
        static REGISTRY: OnceLock<Mutex<HashMap<SourceId, Arc<Probe>>>> = OnceLock::new();
        REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
    }

    fn post_registry() -> &'static Mutex<HashMap<SourceId, Arc<Probe>>> {
        static POST_REGISTRY: OnceLock<Mutex<HashMap<SourceId, Arc<Probe>>>> = OnceLock::new();
        POST_REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
    }

    /// Arm a one-shot rendezvous: the next read to arrive parks after its
    /// admission checks until the test releases it. Later reads find the
    /// release taken and proceed unimpeded.
    pub(crate) fn arm(source: SourceId) -> (Arc<Barrier>, std::sync::mpsc::Sender<()>) {
        let entered = Arc::new(Barrier::new(2));
        let (tx, rx) = std::sync::mpsc::channel();
        registry()
            .lock()
            .expect("read probe registry poisoned")
            .insert(
                source,
                Arc::new(Probe {
                    entered: entered.clone(),
                    release: Mutex::new(Some(rx)),
                }),
            );
        (entered, tx)
    }

    pub(crate) fn disarm(source: &SourceId) {
        registry()
            .lock()
            .expect("read probe registry poisoned")
            .remove(source);
    }

    pub(crate) fn rendezvous(source: &SourceId) {
        let (entered, release) = {
            let registry = registry().lock().expect("read probe registry poisoned");
            match registry.get(source) {
                None => return,
                Some(probe) => (
                    probe.entered.clone(),
                    probe
                        .release
                        .lock()
                        .expect("read probe release poisoned")
                        .take(),
                ),
            }
        };
        if let Some(release) = release {
            entered.wait();
            let _ = release.recv();
        }
    }

    /// Arm the post-poll rendezvous: the next read to finish its post poll
    /// parks after anchor checks pass and before the authority comparison.
    pub(crate) fn arm_post(source: SourceId) -> (Arc<Barrier>, std::sync::mpsc::Sender<()>) {
        let entered = Arc::new(Barrier::new(2));
        let (tx, rx) = std::sync::mpsc::channel();
        post_registry()
            .lock()
            .expect("read post-probe registry poisoned")
            .insert(
                source,
                Arc::new(Probe {
                    entered: entered.clone(),
                    release: Mutex::new(Some(rx)),
                }),
            );
        (entered, tx)
    }

    pub(crate) fn disarm_post(source: &SourceId) {
        post_registry()
            .lock()
            .expect("read post-probe registry poisoned")
            .remove(source);
    }

    pub(crate) fn rendezvous_post(source: &SourceId) {
        let (entered, release) = {
            let registry = post_registry()
                .lock()
                .expect("read post-probe registry poisoned");
            match registry.get(source) {
                None => return,
                Some(probe) => (
                    probe.entered.clone(),
                    probe
                        .release
                        .lock()
                        .expect("read post-probe release poisoned")
                        .take(),
                ),
            }
        };
        if let Some(release) = release {
            entered.wait();
            let _ = release.recv();
        }
    }
}

#[cfg(test)]
mod fixtures {
    //! Shared journal/snapshot builders for the seam tests. One definition
    //! so the two test modules below cannot drift apart on what "a record"
    //! means.
    use super::*;
    use lvu_core::{ChunkPosition, RawRecord, RecordId, StreamKind};
    use lvu_ingest::RuntimeState;

    pub fn test_record(source_id: SourceId, sequence: u64, body: &str) -> RawRecord {
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

    pub fn snapshot(source_id: SourceId, generation: u64, records: u64) -> SourceProgress {
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

    pub fn with_watermark(mut snapshot: SourceProgress, sequence: u64) -> SourceProgress {
        snapshot.high_watermark = Some(RecordId {
            source_id: snapshot.source_id,
            sequence,
        });
        snapshot
    }

    pub fn build_journal(
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
}

#[cfg(test)]
mod tests {
    //! The seam is new; every verdict below is order/content, never timing.
    //! Slow-reader races are resolved by construction (impossible interleaving
    //! asserted nowhere): cancellation stimulation is best-effort while all
    //! assertions hold under every interleaving.
    use super::fixtures::*;
    use super::*;
    use std::time::Duration;

    /// Await a read with a hang guard: every await below completes in
    /// milliseconds on a live runtime, so expiry names a stall location
    /// instead of wedging the gate behind one stuck test.
    async fn guarded_read(
        handle: &RemoteSourceHandle,
        what: &'static str,
    ) -> Result<lvu_core::journal::JournalPage, lvu_ingest::RuntimeError> {
        tokio::time::timeout(
            Duration::from_secs(60),
            handle.read_page(0, 128, 1024 * 1024),
        )
        .await
        .unwrap_or_else(|_| panic!("{what} stalled past its hang guard"))
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
        )
        .expect("bind remote handle");
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
    async fn replacement_invalidates_until_newer_generation_authority() {
        let dir = tempfile::tempdir().unwrap();
        let source_id = SourceId(uuid::Uuid::from_u128(102));
        let journal_path = build_journal(dir.path(), source_id, &["old"]);
        let handle = RemoteSourceHandle::new(
            source_id,
            &journal_path,
            "session-a".into(),
            snapshot(source_id, 1, 1),
            RemoteConfig::default(),
        )
        .expect("bind remote handle");
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
        // Old-generation offsets never return replacement bytes, even though
        // the file is readable: only newer worker-generation authority
        // re-authorizes reads.
        guarded_read(&handle, "invalidated read")
            .await
            .expect_err("invalidated reads must fail, not serve replacement rows");
        // Same-generation ticks keep it failed; a strictly newer generation
        // clears the distrust and serves the new bytes.
        assert!(handle.update_progress("session-a", snapshot(source_id, 1, 1)));
        guarded_read(&handle, "same-generation read")
            .await
            .expect_err("same generation must not clear invalidation");
        assert!(handle.update_progress("session-a", snapshot(source_id, 2, 1)));
        let page = guarded_read(&handle, "re-authorized read")
            .await
            .expect("new generation re-authorizes reads");
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
        )
        .expect("bind remote handle");
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
        )
        .expect("bind remote handle");
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
        )
        .expect("bind remote handle");
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

    /// The worker session a remote handle is bound to, or `None` for local
    /// handles (same-process publication needs no epoch). Feeder and union
    /// transport callers stamp requests with it and detect replacement
    /// through it; the frozen value comes from attach time, never re-read.
    /// Coordinated addition for the union transport + feeder wiring.
    pub fn remote_worker_session(&self) -> Option<&str> {
        match self {
            Self::Local(_) => None,
            Self::Remote(handle) => Some(handle.worker_session()),
        }
    }
}

#[cfg(test)]
mod admission_tests {
    //! Admission and observation-ordering rules, all order/content verdicts.
    //! Pure-admission rulings need no filesystem; every rule that depends on
    //! the journal runs against a real one.
    use super::fixtures::*;
    use super::*;
    use std::time::Duration;

    fn test_source() -> SourceId {
        SourceId(uuid::Uuid::from_u128(201))
    }

    #[test]
    fn foreign_initial_snapshot_fails_construction() {
        let err = RemoteSourceHandle::new(
            test_source(),
            std::path::Path::new("/nonexistent-journal"),
            "session-a".into(),
            snapshot(SourceId(uuid::Uuid::from_u128(202)), 1, 0),
            RemoteConfig::default(),
        )
        .expect_err("foreign initial snapshot must fail");
        assert!(err.contains("another source"), "unexpected message: {err}");
    }

    /// Every admission refusal in rule order — session, source, generation,
    /// records, watermark — then the legitimate moves, against a real
    /// (empty) journal so accepted updates actually publish. No timing
    /// anywhere: refusals return before any poll, acceptances complete
    /// synchronously.
    #[test]
    fn stale_foreign_and_regressed_snapshots_refused_preserving_last_good() {
        let dir = tempfile::tempdir().unwrap();
        let journal_path = dir.path().join("capture.journal");
        std::fs::File::create(&journal_path).unwrap();
        let source = test_source();
        let initial = with_watermark(snapshot(source, 3, 10), 9);
        let handle = RemoteSourceHandle::new(
            source,
            &journal_path,
            "session-a".into(),
            initial,
            RemoteConfig::default(),
        )
        .expect("bind remote handle");
        let stranger = SourceId(uuid::Uuid::from_u128(203));
        // Refusals, each preserving the cache exactly.
        assert!(!handle.update_progress("session-b", snapshot(source, 3, 10)));
        assert!(!handle.update_progress("session-a", snapshot(stranger, 3, 10)));
        assert!(!handle.update_progress("session-a", snapshot(source, 2, 100)));
        assert!(!handle.update_progress("session-a", snapshot(source, 3, 9)));
        assert!(!handle.update_progress("session-a", with_watermark(snapshot(source, 3, 10), 8)));
        let mut gone = snapshot(source, 3, 10);
        gone.high_watermark = None;
        assert!(!handle.update_progress("session-a", gone));
        assert_eq!(handle.progress().generation, 3);
        assert_eq!(handle.progress().records, 10);
        assert_eq!(
            handle.progress().high_watermark.map(|id| id.sequence),
            Some(9)
        );
        // Equal state applies; forward watermark applies.
        assert!(handle.update_progress("session-a", with_watermark(snapshot(source, 3, 10), 9)));
        assert!(handle.update_progress("session-a", with_watermark(snapshot(source, 3, 11), 9)));
        assert_eq!(handle.progress().records, 11);
        // A newer generation is accepted even with fewer records: crash
        // recovery can truncate torn tail bytes, and the bump itself is what
        // re-registers downstream consumers.
        assert!(handle.update_progress("session-a", snapshot(source, 4, 2)));
        assert_eq!(handle.progress().generation, 4);
        assert_eq!(handle.progress().records, 2);
    }

    /// Racing generations share one handle through a barrier start. Updates
    /// carry generation 2 (20 records) or generation 3 (5 records) against a
    /// static file, so every interleaving ends the same way: generation 2
    /// can never overwrite generation 3, because each update validates and
    /// publishes atomically under one guard. A delayed generation-2 update
    /// landing after generation 3 is refused, never stored. This is the
    /// serialized-transaction proof the stamped-observation helper used to
    /// stand in for; the helper is gone with the timestamps it needed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_generation_races_resolve_to_newest() {
        use std::sync::Barrier;
        let dir = tempfile::tempdir().unwrap();
        let source_id = SourceId(uuid::Uuid::from_u128(205));
        let journal_path = build_journal(dir.path(), source_id, &["a", "b", "c"]);
        let handle = RemoteSourceHandle::new(
            source_id,
            &journal_path,
            "session-a".into(),
            snapshot(source_id, 1, 3),
            RemoteConfig::default(),
        )
        .expect("bind remote handle");
        let start = Arc::new(Barrier::new(5));
        let mut tasks = Vec::new();
        for worker in 0..4 {
            let handle = handle.clone();
            let start = start.clone();
            // Even workers push generation 2, odd workers generation 3: the
            // two generations truly race instead of taking turns.
            let (generation, records) = if worker % 2 == 0 { (2, 20) } else { (3, 5) };
            tasks.push(tokio::spawn(async move {
                tokio::task::spawn_blocking(move || start.wait())
                    .await
                    .expect("barrier");
                for _ in 0..10 {
                    handle.update_progress("session-a", snapshot(source_id, generation, records));
                    let page = handle
                        .read_page(0, 128, 1024 * 1024)
                        .await
                        .expect("concurrent read");
                    assert_eq!(page.records.len(), 3);
                }
            }));
        }
        tokio::time::timeout(
            Duration::from_secs(60),
            tokio::task::spawn_blocking({
                let start = start.clone();
                move || start.wait()
            }),
        )
        .await
        .expect("barrier rendezvous without hanging")
        .expect("barrier task");
        for task in tasks {
            tokio::time::timeout(Duration::from_secs(60), task)
                .await
                .expect("worker join without hanging")
                .expect("worker task");
        }
        // Whichever order the forty updates landed in, the cache holds the
        // newest generation: no delayed older update overwrote it, and the
        // file never changed so nothing was classified replaced.
        assert_eq!(handle.progress().generation, 3);
        assert_eq!(handle.progress().records, 5);
        assert_eq!(handle.replacements_observed(), 0);
    }

    /// A replacement landing mid-read discards instead of delivering. The
    /// read parks after its admission checks (deterministic rendezvous, no
    /// timing); the file is then replaced in place at identical length, so
    /// both metadata checks pass and only the authority sample taken beside
    /// the checks can tell the served rows are stale. Without it the resumed
    /// read would return replacement rows under the old authority.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn invalidated_mid_read_discards_replacement_rows() {
        let dir = tempfile::tempdir().unwrap();
        let source_id = SourceId(uuid::Uuid::from_u128(206));
        let journal_path = build_journal(dir.path(), source_id, &["old-0123456789abcdef"]);
        let handle = RemoteSourceHandle::new(
            source_id,
            &journal_path,
            "session-a".into(),
            snapshot(source_id, 1, 1),
            RemoteConfig::default(),
        )
        .expect("bind remote handle");
        assert!(handle.update_progress("session-a", snapshot(source_id, 1, 1)));
        let (entered, release) = read_probe::arm(source_id);
        let reader = handle.clone();
        let parked = tokio::spawn(async move { reader.read_page(0, 128, 1024 * 1024).await });
        // Rendezvous off the runtime worker: the read is parked past its
        // admission checks, holding nothing but its gate permit.
        tokio::time::timeout(
            Duration::from_secs(60),
            tokio::task::spawn_blocking({
                let entered = entered.clone();
                move || entered.wait()
            }),
        )
        .await
        .expect("rendezvous without hanging")
        .expect("barrier task");
        // Replace in place at identical length with different bytes: both
        // metadata checks pass by construction, and the fresh acquisition
        // identity classifies the tick as a replacement.
        let other = tempfile::tempdir().unwrap();
        let other_journal = build_journal(other.path(), source_id, &["new-fedcba9876543210"]);
        let new_bytes = std::fs::read(&other_journal).unwrap();
        assert_eq!(
            new_bytes.len() as u64,
            std::fs::metadata(&journal_path).unwrap().len(),
            "same-length craft keeps the metadata checks blind by design"
        );
        std::fs::write(&journal_path, &new_bytes).unwrap();
        assert!(handle.update_progress("session-a", snapshot(source_id, 1, 1)));
        assert_eq!(handle.replacements_observed(), 1);
        release.send(()).expect("release parked read");
        let outcome = tokio::time::timeout(Duration::from_secs(60), parked)
            .await
            .expect("parked read joins without hanging")
            .expect("parked task");
        assert!(
            outcome.is_err(),
            "mid-read invalidation must discard, not deliver replacement rows"
        );
        read_probe::disarm(&source_id);
        // Newer-generation authority recovers normally afterwards.
        assert!(handle.update_progress("session-a", snapshot(source_id, 2, 1)));
        let page = handle
            .read_page(0, 128, 1024 * 1024)
            .await
            .expect("new generation re-authorizes reads");
        assert_eq!(page.records[0].bytes.as_slice(), b"new-fedcba9876543210");
    }

    #[test]
    fn oversized_and_zero_configs_rejected() {
        assert!(RemoteConfig::new(0, 1024).is_err());
        assert!(RemoteConfig::new(8, 0).is_err());
        assert!(RemoteConfig::new(usize::MAX, 1024).is_err());
        assert!(RemoteConfig::new(8, usize::MAX).is_err());
        assert!(RemoteConfig::new(MAX_PAGE_RECORDS + 1, 1024).is_err());
        assert!(RemoteConfig::new(8, MAX_PAGE_BYTES + 1).is_err());
        RemoteConfig::new(2, 1024).expect("tight but valid ceilings");
    }

    /// Tight ceilings bound reads: at most the configured records per page
    /// however large the request, and requests clamp below the ceiling.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn tight_ceilings_bound_reads_and_requests_clamp() {
        let dir = tempfile::tempdir().unwrap();
        let source_id = SourceId(uuid::Uuid::from_u128(204));
        let journal_path = build_journal(dir.path(), source_id, &["a", "b", "c"]);
        let config = RemoteConfig::new(2, 1024).expect("tight ceilings");
        let handle = RemoteSourceHandle::new(
            source_id,
            &journal_path,
            "session-a".into(),
            snapshot(source_id, 1, 3),
            config,
        )
        .expect("bind remote handle");
        let page = handle
            .read_page(0, 128, 1024 * 1024)
            .await
            .expect("ceiling read");
        assert_eq!(page.records.len(), 2);
        let page = handle
            .read_page(0, 1, 1024 * 1024)
            .await
            .expect("clamped read");
        assert_eq!(page.records.len(), 1);
        assert_eq!(page.records[0].bytes.as_slice(), b"a");
    }

    /// Racing updaters and readers share one handle through a barrier start.
    /// Every update carries the identical snapshot and the file never
    /// changes, so every interleaving has the same verdicts: all updates
    /// apply, all reads are correct, nothing is classified replaced, and
    /// the cache ends exactly where every update put it. Kept alongside the
    /// mixed-generation race below as the pure race-safety half (no panics,
    /// deadlocks, torn state, or lost updates under contention).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_updates_and_reads_stay_coherent() {
        use std::sync::Barrier;
        let dir = tempfile::tempdir().unwrap();
        let source_id = SourceId(uuid::Uuid::from_u128(205));
        let journal_path = build_journal(dir.path(), source_id, &["a", "b", "c"]);
        let handle = RemoteSourceHandle::new(
            source_id,
            &journal_path,
            "session-a".into(),
            snapshot(source_id, 1, 3),
            RemoteConfig::default(),
        )
        .expect("bind remote handle");
        let start = Arc::new(Barrier::new(5));
        let mut tasks = Vec::new();
        for _ in 0..4 {
            let handle = handle.clone();
            let start = start.clone();
            tasks.push(tokio::spawn(async move {
                tokio::task::spawn_blocking(move || start.wait())
                    .await
                    .expect("barrier");
                for _ in 0..25 {
                    assert!(handle.update_progress("session-a", snapshot(source_id, 1, 3)));
                    let page = handle
                        .read_page(0, 128, 1024 * 1024)
                        .await
                        .expect("concurrent read");
                    assert_eq!(page.records.len(), 3);
                }
            }));
        }
        tokio::time::timeout(
            Duration::from_secs(60),
            tokio::task::spawn_blocking({
                let start = start.clone();
                move || start.wait()
            }),
        )
        .await
        .expect("barrier rendezvous without hanging")
        .expect("barrier task");
        for task in tasks {
            tokio::time::timeout(Duration::from_secs(60), task)
                .await
                .expect("worker join without hanging")
                .expect("worker task");
        }
        assert_eq!(handle.replacements_observed(), 0);
        assert_eq!(handle.progress().records, 3);
        assert_eq!(handle.progress().generation, 1);
    }
}

#[cfg(test)]
mod authority_tests {
    //! New-generation authority and first-read establishment, both
    //! order/content verdicts with no timing and no ticks where none are
    //! needed.
    use super::fixtures::*;
    use super::*;
    use lvu_core::SourceId;
    use std::time::Duration;

    /// Replacement first sighted together with a newer generation adopts it
    /// at once: the generation-2 snapshot IS the new authority describing
    /// the restarted capture, so the update succeeds and the very next read
    /// serves the new bytes with no generation 3 required. Holding the
    /// update for a further generation here would stall the live tail one
    /// full restart behind reality.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn replacement_with_newer_generation_authorizes_immediately() {
        let dir = tempfile::tempdir().unwrap();
        let source_id = SourceId(uuid::Uuid::from_u128(207));
        let journal_path = build_journal(dir.path(), source_id, &["old"]);
        let handle = RemoteSourceHandle::new(
            source_id,
            &journal_path,
            "session-a".into(),
            snapshot(source_id, 1, 1),
            RemoteConfig::default(),
        )
        .expect("bind remote handle");
        assert!(handle.update_progress("session-a", snapshot(source_id, 1, 1)));
        std::fs::remove_file(&journal_path).unwrap();
        build_journal(dir.path(), source_id, &["new-content-longer"]);
        // First sighting carries generation 2: adopt, do not distrust.
        assert!(handle.update_progress("session-a", snapshot(source_id, 2, 1)));
        assert_eq!(handle.replacements_observed(), 1);
        assert_eq!(handle.progress().generation, 2);
        let page = handle
            .read_page(0, 128, 1024 * 1024)
            .await
            .expect("new authority serves immediately");
        assert_eq!(page.records[0].bytes.as_slice(), b"new-content-longer");
    }

    /// Reads before the first tick still fence: the first read establishes
    /// the remembered filesystem state under the state transaction, so a
    /// replacement landing before any tick fails the second read instead of
    /// serving new bytes under no authority at all.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn first_read_establishes_state_for_later_reads() {
        let dir = tempfile::tempdir().unwrap();
        let source_id = SourceId(uuid::Uuid::from_u128(208));
        let journal_path = build_journal(dir.path(), source_id, &["old"]);
        let handle = RemoteSourceHandle::new(
            source_id,
            &journal_path,
            "session-a".into(),
            snapshot(source_id, 1, 1),
            RemoteConfig::default(),
        )
        .expect("bind remote handle");
        // No update_progress call anywhere in this test: no tick ever runs.
        let page = handle
            .read_page(0, 128, 1024 * 1024)
            .await
            .expect("first read establishes");
        assert_eq!(page.records[0].bytes.as_slice(), b"old");
        std::fs::remove_file(&journal_path).unwrap();
        build_journal(dir.path(), source_id, &["new-content-longer"]);
        handle
            .read_page(0, 128, 1024 * 1024)
            .await
            .expect_err("second read must fail against established state");
    }

    /// Pre-tick same-inode equal-length replacement must fail without any
    /// tick: identity and length do not move, so only the content anchor
    /// can tell the second read it is looking at a different capture.
    /// Overwrites in place (`write`, same inode) with a different valid
    /// journal of exactly equal serialized length and a different
    /// acquisition identity; both metadata fences pass by construction.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn pre_tick_same_length_in_place_replacement_fails() {
        let dir = tempfile::tempdir().unwrap();
        let source_id = SourceId(uuid::Uuid::from_u128(209));
        let journal_path = build_journal(dir.path(), source_id, &["old-0123456789abcdef"]);
        let handle = RemoteSourceHandle::new(
            source_id,
            &journal_path,
            "session-a".into(),
            snapshot(source_id, 1, 1),
            RemoteConfig::default(),
        )
        .expect("bind remote handle");
        // No tick anywhere before the replacement: the first read alone
        // establishes the remembered anchor.
        let page = handle
            .read_page(0, 128, 1024 * 1024)
            .await
            .expect("first read establishes");
        assert_eq!(page.records[0].bytes.as_slice(), b"old-0123456789abcdef");
        let other = tempfile::tempdir().unwrap();
        let other_journal = build_journal(other.path(), source_id, &["new-fedcba9876543210"]);
        let new_bytes = std::fs::read(&other_journal).unwrap();
        assert_eq!(
            new_bytes.len() as u64,
            std::fs::metadata(&journal_path).unwrap().len(),
            "same-length craft keeps identity/length blind by design"
        );
        std::fs::write(&journal_path, &new_bytes).unwrap();
        handle
            .read_page(0, 128, 1024 * 1024)
            .await
            .expect_err("same-inode equal-length pre-tick replacement must fail");
    }

    /// Newer-generation adoption still moves the authority epoch: a read
    /// admitted before adoption sampled the old epoch and must discard,
    /// while reads starting after adoption sample the new epoch and serve
    /// the new bytes at once. Same-inode equal-length craft keeps both
    /// metadata fences blind by design, so only the epoch can fence the
    /// parked read.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn newer_generation_adoption_discards_in_flight_read() {
        let dir = tempfile::tempdir().unwrap();
        let source_id = SourceId(uuid::Uuid::from_u128(210));
        let journal_path = build_journal(dir.path(), source_id, &["old-0123456789abcdef"]);
        let handle = RemoteSourceHandle::new(
            source_id,
            &journal_path,
            "session-a".into(),
            snapshot(source_id, 1, 1),
            RemoteConfig::default(),
        )
        .expect("bind remote handle");
        assert!(handle.update_progress("session-a", snapshot(source_id, 1, 1)));
        let (entered, release) = read_probe::arm(source_id);
        let reader = handle.clone();
        let parked = tokio::spawn(async move { reader.read_page(0, 128, 1024 * 1024).await });
        tokio::time::timeout(
            Duration::from_secs(60),
            tokio::task::spawn_blocking({
                let entered = entered.clone();
                move || entered.wait()
            }),
        )
        .await
        .expect("rendezvous without hanging")
        .expect("barrier task");
        let other = tempfile::tempdir().unwrap();
        let other_journal = build_journal(other.path(), source_id, &["new-fedcba9876543210"]);
        let new_bytes = std::fs::read(&other_journal).unwrap();
        assert_eq!(
            new_bytes.len() as u64,
            std::fs::metadata(&journal_path).unwrap().len(),
            "same-length craft keeps the metadata checks blind by design"
        );
        std::fs::write(&journal_path, &new_bytes).unwrap();
        // First sighting carries generation 2: adopts immediately and moves
        // the epoch, so the parked generation-1 read discards.
        assert!(handle.update_progress("session-a", snapshot(source_id, 2, 1)));
        assert_eq!(handle.replacements_observed(), 1);
        assert_eq!(handle.progress().generation, 2);
        release.send(()).expect("release parked read");
        let outcome = tokio::time::timeout(Duration::from_secs(60), parked)
            .await
            .expect("parked read joins without hanging")
            .expect("parked task");
        assert!(
            outcome.is_err(),
            "in-flight read across newer-generation adoption must discard"
        );
        read_probe::disarm(&source_id);
        let page = handle
            .read_page(0, 128, 1024 * 1024)
            .await
            .expect("new authority serves immediately");
        assert_eq!(page.records[0].bytes.as_slice(), b"new-fedcba9876543210");
    }

    /// Invalid gen-1 followed by a second replacement first seen with gen-2
    /// must serve immediately without a third tick: the gen-2 adoption both
    /// moves the epoch and clears the gen-1 distrust. Suppressing the clear
    /// when the tick already bumped leaves `Some(1)` behind and fails the
    /// immediate read.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn invalid_gen1_then_replacement_gen2_serves_immediately() {
        let dir = tempfile::tempdir().unwrap();
        let source_id = SourceId(uuid::Uuid::from_u128(211));
        let journal_path = build_journal(dir.path(), source_id, &["old-0123456789abcdef"]);
        let handle = RemoteSourceHandle::new(
            source_id,
            &journal_path,
            "session-a".into(),
            snapshot(source_id, 1, 1),
            RemoteConfig::default(),
        )
        .expect("bind remote handle");
        assert!(handle.update_progress("session-a", snapshot(source_id, 1, 1)));
        // First replacement, same generation: distrust gen-1.
        let other = tempfile::tempdir().unwrap();
        let mid_journal = build_journal(other.path(), source_id, &["mid-0123456789abcdef"]);
        let mid_bytes = std::fs::read(&mid_journal).unwrap();
        assert_eq!(
            mid_bytes.len() as u64,
            std::fs::metadata(&journal_path).unwrap().len(),
            "same-length craft keeps identity/length blind by design"
        );
        std::fs::write(&journal_path, &mid_bytes).unwrap();
        assert!(handle.update_progress("session-a", snapshot(source_id, 1, 1)));
        assert_eq!(handle.replacements_observed(), 1);
        handle
            .read_page(0, 128, 1024 * 1024)
            .await
            .expect_err("gen-1 distrust must fail reads");
        // Second replacement, first seen with gen-2: adopts, moves the epoch
        // once, and clears the gen-1 distrust so the immediate read serves.
        let other2 = tempfile::tempdir().unwrap();
        let new_journal = build_journal(other2.path(), source_id, &["new-fedcba9876543210"]);
        let new_bytes = std::fs::read(&new_journal).unwrap();
        assert_eq!(
            new_bytes.len() as u64,
            std::fs::metadata(&journal_path).unwrap().len(),
            "same-length craft keeps identity/length blind by design"
        );
        std::fs::write(&journal_path, &new_bytes).unwrap();
        assert!(handle.update_progress("session-a", snapshot(source_id, 2, 1)));
        assert_eq!(handle.replacements_observed(), 2);
        assert_eq!(handle.progress().generation, 2);
        let page = handle
            .read_page(0, 128, 1024 * 1024)
            .await
            .expect("gen-2 adoption after gen-1 distrust serves without a third tick");
        assert_eq!(page.records[0].bytes.as_slice(), b"new-fedcba9876543210");
    }

    /// Epoch isolation: parks after `pre`/`post` are both taken (both old,
    /// anchor checks pass by construction), so only the authority counter
    /// can fail the released read. The pre-service rendezvous above cannot
    /// prove this — its post anchor already rejects — which is why this
    /// post-poll rendezvous exists.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn post_poll_epoch_alone_discards_parked_read() {
        let dir = tempfile::tempdir().unwrap();
        let source_id = SourceId(uuid::Uuid::from_u128(212));
        let journal_path = build_journal(dir.path(), source_id, &["old-0123456789abcdef"]);
        let handle = RemoteSourceHandle::new(
            source_id,
            &journal_path,
            "session-a".into(),
            snapshot(source_id, 1, 1),
            RemoteConfig::default(),
        )
        .expect("bind remote handle");
        assert!(handle.update_progress("session-a", snapshot(source_id, 1, 1)));
        let (entered, release) = read_probe::arm_post(source_id);
        let reader = handle.clone();
        let parked = tokio::spawn(async move { reader.read_page(0, 128, 1024 * 1024).await });
        tokio::time::timeout(
            Duration::from_secs(60),
            tokio::task::spawn_blocking({
                let entered = entered.clone();
                move || entered.wait()
            }),
        )
        .await
        .expect("post rendezvous without hanging")
        .expect("barrier task");
        // Replacement lands after `post` was taken: `pre`/`post` are both
        // old, so anchor checks pass and only the epoch move can discard.
        let other = tempfile::tempdir().unwrap();
        let other_journal = build_journal(other.path(), source_id, &["new-fedcba9876543210"]);
        let new_bytes = std::fs::read(&other_journal).unwrap();
        assert_eq!(
            new_bytes.len() as u64,
            std::fs::metadata(&journal_path).unwrap().len(),
            "same-length craft keeps the metadata checks blind by design"
        );
        std::fs::write(&journal_path, &new_bytes).unwrap();
        assert!(handle.update_progress("session-a", snapshot(source_id, 2, 1)));
        assert_eq!(handle.progress().generation, 2);
        release.send(()).expect("release post-parked read");
        let outcome = tokio::time::timeout(Duration::from_secs(60), parked)
            .await
            .expect("post-parked read joins without hanging")
            .expect("parked task");
        assert!(
            outcome.is_err(),
            "post-poll parked read must discard via epoch alone"
        );
        read_probe::disarm_post(&source_id);
        let page = handle
            .read_page(0, 128, 1024 * 1024)
            .await
            .expect("new authority serves immediately");
        assert_eq!(page.records[0].bytes.as_slice(), b"new-fedcba9876543210");
    }

    /// Publication slot stays canonical under races: concurrent updaters and
    /// UI readers share one handle, every `progress()` read is a complete
    /// accepted snapshot (never torn, never foreign, never a refused stale),
    /// and the newest generation wins. The UI getter takes only the
    /// publication lock, never the IO-guarded state mutex, so readers never
    /// wedge on a poll.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_progress_readers_see_only_accepted() {
        use std::sync::Barrier;
        let dir = tempfile::tempdir().unwrap();
        let source_id = SourceId(uuid::Uuid::from_u128(213));
        let journal_path = build_journal(dir.path(), source_id, &["a", "b", "c"]);
        let handle = RemoteSourceHandle::new(
            source_id,
            &journal_path,
            "session-a".into(),
            snapshot(source_id, 1, 3),
            RemoteConfig::default(),
        )
        .expect("bind remote handle");
        let start = Arc::new(Barrier::new(5));
        let mut tasks = Vec::new();
        for worker in 0..4 {
            let handle = handle.clone();
            let start = start.clone();
            let (generation, records) = if worker % 2 == 0 { (2, 20) } else { (3, 5) };
            tasks.push(tokio::spawn(async move {
                tokio::task::spawn_blocking(move || start.wait())
                    .await
                    .expect("barrier");
                for _ in 0..25 {
                    handle.update_progress("session-a", snapshot(source_id, generation, records));
                    let published = handle.progress();
                    assert_eq!(published.source_id, source_id);
                    assert!(matches!(published.generation, 1..=3));
                    match published.generation {
                        1 => assert_eq!(published.records, 3),
                        2 => assert_eq!(published.records, 20),
                        3 => assert_eq!(published.records, 5),
                        _ => unreachable!(),
                    }
                }
            }));
        }
        tokio::time::timeout(
            Duration::from_secs(60),
            tokio::task::spawn_blocking({
                let start = start.clone();
                move || start.wait()
            }),
        )
        .await
        .expect("barrier rendezvous without hanging")
        .expect("barrier task");
        for task in tasks {
            tokio::time::timeout(Duration::from_secs(60), task)
                .await
                .expect("worker join without hanging")
                .expect("worker task");
        }
        assert_eq!(handle.progress().generation, 3);
        assert_eq!(handle.progress().records, 5);
        // Refused stale never publishes: delayed gen-2 cannot overwrite gen-3.
        assert!(!handle.update_progress("session-a", snapshot(source_id, 2, 20)));
        assert_eq!(handle.progress().generation, 3);
        assert_eq!(handle.progress().records, 5);
    }
}
