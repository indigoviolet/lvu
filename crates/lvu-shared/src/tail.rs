//! Read-only journal tail for windows attached to a worker.
//!
//! A window never opens a journal for writing: [`FileJournalTail`] opens a
//! fresh [`JournalReader`](lvu_core::journal::JournalReader) per page, which
//! takes no lock and disturbs no writer. Append-vs-replacement is decided
//! by [`classify`] over filesystem identity, offset continuity, and a
//! content anchor — never by change time, which moves on every flushed
//! append and therefore cannot disambiguate anything. Adapters own
//! generation fencing on top of [`Continuity`]; this module only reports
//! observable facts.

use std::{
    io,
    path::{Path, PathBuf},
};

use lvu_core::{
    SourceId,
    journal::{JournalPage, JournalReader},
};

/// A read-only handle on one capture journal. Cheaply cloneable; every page
/// read opens its own reader, so concurrent readers never share mutable
/// state and a stuck reader cannot wedge the others.
#[derive(Clone, Debug)]
pub struct FileJournalTail {
    source_id: SourceId,
    journal_path: PathBuf,
}

/// What polling a journal file can observe. Everything here is a fact about
/// bytes on disk at poll time, never an interpretation. The anchor is the
/// first record's stable identity plus its byte length: cheap (one bounded
/// single-record read) and sufficient, because any replacement that keeps
/// both the filesystem identity and the entire first record is
/// byte-identical where observed and therefore indistinguishable from
/// continuity by any means available.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TailStatus {
    pub source_id: SourceId,
    pub file_len: u64,
    pub identity: FileIdentity,
    pub anchor: Option<RecordAnchor>,
    pub writer_present: bool,
}

/// The content anchor: who the first record claims to be and how long it
/// is. Compared by value, never by display rendering.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordAnchor {
    pub acquisition_id: uuid::Uuid,
    pub first_record_len: usize,
}

/// Filesystem identity for restart detection: device and inode only. This
/// host's filesystem demonstrably recycles inode numbers on
/// delete+recreate, so identity alone never proves continuity either —
/// [`classify`] always consults length and anchor as well. Change time is
/// deliberately absent: it moves on every flushed append.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileIdentity {
    pub device: u64,
    pub inode: u64,
}

impl FileIdentity {
    pub fn of(path: &Path) -> io::Result<(Self, u64)> {
        let metadata = std::fs::metadata(path)?;
        Ok((Self::from_metadata(&metadata)?, metadata.len()))
    }

    #[cfg(unix)]
    fn from_metadata(metadata: &std::fs::Metadata) -> io::Result<Self> {
        use std::os::unix::fs::MetadataExt;
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }

    #[cfg(not(unix))]
    fn from_metadata(metadata: &std::fs::Metadata) -> io::Result<Self> {
        // No device/inode portably: length stands in, so replacement by an
        // equal-length file reads as continuity unless the anchor differs.
        // Documented weakness; acceptable only because record identities
        // still fence consumers.
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        metadata.len().hash(&mut hasher);
        metadata
            .modified()
            .map_err(|error| {
                io::Error::new(io::ErrorKind::Other, format!("mtime unavailable: {error}"))
            })?
            .hash(&mut hasher);
        Ok(Self {
            device: 0,
            inode: hasher.finish(),
        })
    }
}

/// Append vs replacement, decided from two consecutive polls.
///
/// - Filesystem identity changed → replaced.
/// - Length shrank → replaced (truncation is never an append).
/// - Anchor changed → replaced, even at identical identity and length.
/// - Otherwise continuous. In particular identical identity, length, and
///   anchor is continuity: same-inode reuse with byte-identical observed
///   content has no observable difference to fence on, and none is needed.
///
/// An unreadable anchor with a nonzero length is treated as replaced: the
/// only way record zero becomes unreadable while bytes exist is a torn
/// replacement racing the poll, and re-fencing caches then is cheap while
/// missing a replacement is not. Empty on both sides is continuous.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Continuity {
    Continuous,
    Replaced,
}

pub fn classify(previous: &TailStatus, current: &TailStatus) -> Continuity {
    if previous.identity != current.identity {
        return Continuity::Replaced;
    }
    if current.file_len < previous.file_len {
        return Continuity::Replaced;
    }
    match (&previous.anchor, &current.anchor) {
        (Some(previous), Some(current)) if previous != current => Continuity::Replaced,
        (Some(_), None) if current.file_len > 0 => Continuity::Replaced,
        _ => Continuity::Continuous,
    }
}

impl FileJournalTail {
    pub fn new(source_id: SourceId, journal_path: &Path) -> Self {
        Self {
            source_id,
            journal_path: journal_path.to_path_buf(),
        }
    }

    pub fn source_id(&self) -> SourceId {
        self.source_id
    }

    pub fn journal_path(&self) -> &Path {
        &self.journal_path
    }

    /// Poll observable facts: length, identity, content anchor, and whether
    /// a writer holds the journal right now (via `writer_present`, which
    /// never disturbs). The anchor read is one bounded single-record page;
    /// an unreadable anchor on a nonempty file reports `None` and lets
    /// [`classify`] treat it as a replacement (cheap re-fence over a
    /// missed restart).
    pub fn poll_status(&self) -> io::Result<TailStatus> {
        let (identity, file_len) = FileIdentity::of(&self.journal_path)?;
        let anchor = self.read_anchor();
        let writer_present = lvu_core::journal::writer_present(&self.journal_path)
            .map_err(|error| io::Error::other(format!("writer probe failed: {error}")))?;
        Ok(TailStatus {
            source_id: self.source_id,
            file_len,
            identity,
            anchor,
            writer_present,
        })
    }

    /// Read the content anchor: the first record's stable acquisition
    /// identity plus its byte length. `None` when the journal is empty or
    /// the head cannot be read (torn replacement racing the poll).
    pub fn read_anchor(&self) -> Option<RecordAnchor> {
        let page = self.read_page(0, 1, 64 * 1024).ok()?;
        let record = page.records.first()?;
        Some(RecordAnchor {
            acquisition_id: record.acquisition_id,
            first_record_len: record.bytes.as_slice().len(),
        })
    }

    /// Read one page through a fresh reader: no locks taken, no writer
    /// disturbed, no shared mutable state.
    pub fn read_page(
        &self,
        offset: u64,
        max_records: usize,
        max_bytes: usize,
    ) -> Result<JournalPage, lvu_core::journal::JournalError> {
        let mut reader = JournalReader::open(&self.journal_path, self.source_id)?;
        reader.read_page(offset, max_records, max_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lvu_core::{ChunkPosition, RawRecord, RecordId, StreamKind};

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

    fn test_id(n: u128) -> SourceId {
        SourceId(uuid::Uuid::from_u128(n))
    }

    /// A spawned fixture that is always reaped: drop kills and waits, so a
    /// failing assertion never leaves a sleeper behind.
    struct Reap(std::process::Child);

    impl Drop for Reap {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn tail_reads_written_records_in_order() {
        let directory = tempfile::tempdir().unwrap();
        let journal_path = directory.path().join("capture.journal");
        let source_id = test_id(1);
        let (mut journal, _) = lvu_core::journal::Journal::open(&journal_path, source_id).unwrap();
        for sequence in 0..3u64 {
            journal
                .append(test_record(source_id, sequence, &format!("row-{sequence}")))
                .unwrap();
        }
        journal.flush().unwrap();
        let tail = FileJournalTail::new(source_id, &journal_path);
        let page = tail.read_page(0, 128, 1024 * 1024).unwrap();
        assert_eq!(page.records.len(), 3);
        assert_eq!(page.records[2].record_id.sequence, 2);
        assert_eq!(page.records[0].bytes.as_slice(), b"row-0");
        let status = tail.poll_status().unwrap();
        assert!(status.file_len > 0);
        // The writer is open in this process: the probe answers from the
        // claim registry without touching the file.
        assert!(status.writer_present);
    }

    #[test]
    fn replacement_detected_by_anchor_not_identity_alone() {
        let directory = tempfile::tempdir().unwrap();
        let journal_path = directory.path().join("capture.journal");
        let source_id = test_id(2);
        let (mut journal, _) = lvu_core::journal::Journal::open(&journal_path, source_id).unwrap();
        journal.append(test_record(source_id, 0, "first")).unwrap();
        journal.flush().unwrap();
        let tail = FileJournalTail::new(source_id, &journal_path);
        let before = tail.poll_status().unwrap();
        assert!(before.anchor.is_some());
        // Append under the same file: continuous, anchor stable.
        journal.append(test_record(source_id, 1, "more")).unwrap();
        journal.flush().unwrap();
        let appended = tail.poll_status().unwrap();
        assert_eq!(classify(&before, &appended), Continuity::Continuous);
        // Replace the file (drop, remove, recreate with a fresh acquisition
        // identity): replaced however the filesystem recycles the inode.
        drop(journal);
        std::fs::remove_file(&journal_path).unwrap();
        let (mut journal, _) = lvu_core::journal::Journal::open(&journal_path, source_id).unwrap();
        journal.append(test_record(source_id, 0, "second")).unwrap();
        journal.flush().unwrap();
        let after = tail.poll_status().unwrap();
        assert_eq!(classify(&before, &after), Continuity::Replaced);
        let page = tail.read_page(0, 128, 1024 * 1024).unwrap();
        assert_eq!(page.records[0].bytes.as_slice(), b"second");
        // Truncation to empty reads as replacement, never continuity.
        drop(journal);
        std::fs::File::create(&journal_path)
            .unwrap()
            .set_len(0)
            .unwrap();
        let truncated = tail.poll_status().unwrap();
        assert_eq!(classify(&after, &truncated), Continuity::Replaced);
    }

    #[test]
    fn classify_matrix_covers_same_inode_reuse() {
        fn status(device: u64, inode: u64, len: u64, anchor: Option<(u128, usize)>) -> TailStatus {
            TailStatus {
                source_id: test_id(9),
                file_len: len,
                identity: FileIdentity { device, inode },
                anchor: anchor.map(|(id, len)| RecordAnchor {
                    acquisition_id: uuid::Uuid::from_u128(id),
                    first_record_len: len,
                }),
                writer_present: false,
            }
        }
        let live = status(7, 7, 100, Some((1, 10)));
        // Identical observation is continuity.
        assert_eq!(classify(&live, &live), Continuity::Continuous);
        // Same inode, same length, different anchor: the recycled-inode
        // replacement. Identity and length alone cannot see it; the anchor can.
        let replaced_same_inode = status(7, 7, 100, Some((2, 10)));
        assert_eq!(classify(&live, &replaced_same_inode), Continuity::Replaced);
        // Same inode, same anchor, grown length: ordinary append.
        let appended = status(7, 7, 200, Some((1, 10)));
        assert_eq!(classify(&live, &appended), Continuity::Continuous);
        // New inode: replacement regardless of content.
        let moved = status(7, 8, 100, Some((1, 10)));
        assert_eq!(classify(&live, &moved), Continuity::Replaced);
        // Shorter file: truncation, never an append.
        let truncated = status(7, 7, 50, Some((1, 10)));
        assert_eq!(classify(&live, &truncated), Continuity::Replaced);
        // Unreadable anchor on a nonempty file: torn replacement racing the
        // poll; re-fence rather than miss it.
        let torn = status(7, 7, 100, None);
        assert_eq!(classify(&live, &torn), Continuity::Replaced);
        // Empty on both sides: nothing to fence.
        let empty = status(7, 7, 0, None);
        assert_eq!(classify(&empty, &empty), Continuity::Continuous);
        // First records arriving is continuity, not replacement.
        assert_eq!(classify(&empty, &live), Continuity::Continuous);
    }

    /// Cross-process writer fixture: when `LVU_SHARED_TEST_ROLE=writer` the
    /// current test binary acts as a live journal writer in a CHILD process
    /// (real record locks, real file growth), so the parent exercises the
    /// tail against a genuinely foreign writer. Without the env marker this
    /// test is the parent side.
    #[test]
    fn tail_reads_live_writer_across_processes() {
        if std::env::var("LVU_SHARED_TEST_ROLE").as_deref() == Ok("writer") {
            writer_child_main();
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let journal_path = directory.path().join("capture.journal");
        let source_id = test_id(3);
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            // Substring filter (no --exact, which needs the full module
            // path): this name is unique in the binary.
            .arg("tail_reads_live_writer_across_processes")
            .arg("--nocapture")
            .arg("--test-threads")
            .arg("1")
            .env("LVU_SHARED_TEST_ROLE", "writer")
            .env("LVU_SHARED_JOURNAL", &journal_path)
            .env("LVU_SHARED_SOURCE", source_id.0.to_string())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn writer child");
        let child = Reap(child);
        let tail = FileJournalTail::new(source_id, &journal_path);
        // Wait for all five records with a deadline; no sleeps-as-sync, only
        // readiness polling with a hard cap.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        let page = loop {
            if let Ok(page) = tail.read_page(0, 128, 1024 * 1024)
                && page.records.len() >= 5
            {
                break page;
            }
            if std::time::Instant::now() >= deadline {
                panic!("live writer records never arrived");
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        };
        for (index, record) in page.records.iter().enumerate() {
            assert_eq!(record.record_id.sequence, index as u64);
        }
        // While the child lives, the probe reports a foreign writer without
        // disturbing it.
        assert!(tail.poll_status().unwrap().writer_present);
        // Reap (kills if still running) and prove the writer is gone while
        // every committed record remains readable.
        drop(child);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if let Ok(status) = tail.poll_status()
                && !status.writer_present
            {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!("dead writer still reported present");
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        let page = tail.read_page(0, 128, 1024 * 1024).unwrap();
        assert!(page.records.len() >= 5);
    }

    fn writer_child_main() {
        let journal_path =
            std::env::var("LVU_SHARED_JOURNAL").expect("writer child needs a journal path");
        let source_id = lvu_core::SourceId(
            uuid::Uuid::parse_str(
                &std::env::var("LVU_SHARED_SOURCE").expect("writer child needs a source id"),
            )
            .expect("valid source id"),
        );
        let (mut journal, _) =
            lvu_core::journal::Journal::open(journal_path, source_id).expect("child opens journal");
        for sequence in 0..5u64 {
            journal
                .append(test_record(
                    source_id,
                    sequence,
                    &format!("live-{sequence}"),
                ))
                .expect("child appends");
            journal.flush().expect("child flushes");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}
