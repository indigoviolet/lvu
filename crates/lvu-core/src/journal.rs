use crate::{ChunkPosition, RawRecord, RecordId, SourceId, StreamKind};
use crc32fast::hash;
use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{self, BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};
use thiserror::Error;
use uuid::Uuid;

const MAGIC: &[u8; 4] = b"LVU2";
const HEADER: usize = 16;
const FIXED: usize = 58;
const MAX_FRAME: usize = 2 * 1024 * 1024;
const SEQUENCE_BLOCK: u64 = 1024;

#[derive(Debug, Error)]
pub enum JournalError {
    #[error("journal I/O: {0}")]
    Io(#[from] io::Error),
    #[error("committed journal corruption at byte {offset}: {reason}")]
    Corrupt { offset: u64, reason: &'static str },
    #[error("journal already has an owning writer")]
    AlreadyOpen,
    #[error("journal writer is poisoned; reopen to recover its incomplete tail")]
    Poisoned,
    #[error("record source does not match journal")]
    WrongSource,
    #[error("record payload exceeds journal frame limit")]
    FrameTooLarge,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Recovery {
    pub records: u64,
    pub truncated_tail_bytes: u64,
    pub next_sequence: u64,
    pub last_sequence: Option<u64>,
}

/// A wall-clock ring of journal lifecycle events, dumped only when an open is
/// refused.
///
/// Printing each event changed the timing enough to hide the race it was meant
/// to explain, so events are appended to a bounded buffer and the buffer is
/// emitted once, at the failure. Off unless `LVU_JOURNAL_TRACE` is set.
pub mod trace {
    use std::sync::{Mutex, OnceLock};
    use std::time::Instant;

    const CAPACITY: usize = 512;

    struct Ring {
        started: Instant,
        events: Vec<(f64, String)>,
        next: usize,
    }

    fn ring() -> Option<&'static Mutex<Ring>> {
        static RING: OnceLock<Option<Mutex<Ring>>> = OnceLock::new();
        RING.get_or_init(|| {
            std::env::var_os("LVU_JOURNAL_TRACE").map(|_| {
                Mutex::new(Ring {
                    started: Instant::now(),
                    events: Vec::with_capacity(CAPACITY),
                    next: 0,
                })
            })
        })
        .as_ref()
    }

    /// True when tracing is on, so callers can skip formatting otherwise.
    pub fn enabled() -> bool {
        ring().is_some()
    }

    pub fn record(event: impl FnOnce() -> String) {
        let Some(ring) = ring() else { return };
        let Ok(mut ring) = ring.lock() else { return };
        let at = ring.started.elapsed().as_secs_f64();
        let entry = (at, event());
        // A true ring: shifting a full vector on every event cost enough time
        // to close the window this is trying to observe.
        if ring.events.len() < CAPACITY {
            ring.events.push(entry);
        } else {
            let slot = ring.next;
            ring.events[slot] = entry;
        }
        ring.next = (ring.next + 1) % CAPACITY;
    }

    pub fn dump(reason: &str) {
        let Some(ring) = ring() else { return };
        let Ok(ring) = ring.lock() else { return };
        eprintln!(
            "JOURNAL-TRACE dump ({reason}), {} events:",
            ring.events.len()
        );
        let mut ordered: Vec<&(f64, String)> = ring.events.iter().collect();
        ordered.sort_by(|a, b| a.0.total_cmp(&b.0));
        for (at, event) in ordered {
            eprintln!("  {at:9.6}s {event}");
        }
    }
}

#[derive(Debug)]
pub struct JournalPage {
    pub records: Vec<RawRecord>,
    pub next_offset: u64,
    pub end_of_journal: bool,
}

pub struct JournalReader {
    source_id: SourceId,
    file: File,
}

impl JournalReader {
    pub fn open(path: impl AsRef<Path>, source_id: SourceId) -> Result<Self, JournalError> {
        Ok(Self {
            source_id,
            file: OpenOptions::new().read(true).open(path)?,
        })
    }

    pub fn read_page(
        &mut self,
        offset: u64,
        max_records: usize,
        max_bytes: usize,
    ) -> Result<JournalPage, JournalError> {
        read_page_from(
            &mut self.file,
            self.source_id,
            offset,
            max_records,
            max_bytes,
        )
    }
}

#[derive(Clone, Copy)]
struct ScanMetadata {
    records: u64,
    valid_len: u64,
    maximum_sequence: Option<u64>,
}

// Exclusive ownership of a journal, split across the two ways it can be lost.
//
// The obvious implementation — one `flock` on `<journal>.lock` — has a defect
// that only shows up on a machine under load, and it is not ours to fix in the
// kernel: an `flock` belongs to the *open file description*, and `fork` hands
// every child a duplicate of it. Between the fork and the `exec` that closes
// it, a child of this process holds our journal lock. If the journal is
// dropped in that window, the lock outlives it, and the next open of the same
// source is refused with `AlreadyOpen` even though nothing owns the journal.
//
// That window is wide here: `configure_owned_process` installs a `pre_exec`
// hook, which takes std off `posix_spawn` and onto a plain `fork`, and the
// child then runs `prctl` before it execs. Every command source lvu spawns
// duplicates the descriptor table, so a source that restarts while any command
// source is starting can lose the race. Measured: an identical 200-cycle
// restart loop with four threads spawning subprocesses failed 11 runs in 20 at
// load 23; the same loop with no subprocess passed 20 in 20.
//
// So ownership is asserted by two mechanisms, each doing what it is good at
// and neither depending on descriptor inheritance:
//
// * In this process, a registry of claimed lock paths. It is exact — a second
//   open sees the first one's claim, not a syscall's opinion — and a forked
//   child gets a copy of memory it never runs.
// * Across processes, a POSIX record lock (`F_SETLK`). Unlike `flock`, a
//   record lock is owned by the *process*, and `fork(2)` does not pass it to
//   the child, which closes the window entirely. It is still released by the
//   kernel if we crash, so a killed lvu does not strand its own journal.
//
// The record lock's one sharp edge is that it dies when *this process* closes
// any descriptor on the lock file, not just the one that took it. That is why
// the claim is taken before the file is opened: a refused open must not be
// able to open, and then close, the file whose lock an incumbent journal is
// relying on.
/// Every journal lock path this process currently owns.
fn claimed_paths() -> &'static Mutex<HashSet<PathBuf>> {
    static CLAIMED: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    CLAIMED.get_or_init(|| Mutex::new(HashSet::new()))
}

/// A lock path claimed by this process, released when the journal drops.
struct PathClaim {
    path: PathBuf,
}

impl PathClaim {
    /// `None` when another `Journal` in this process already holds the path.
    fn take(lock_path: &Path) -> Option<Self> {
        let path = normalized(lock_path);
        let mut claimed = claimed_paths().lock().unwrap_or_else(|error| {
            claimed_paths().clear_poison();
            error.into_inner()
        });
        let taken = claimed.insert(path.clone());
        // The claim is built after the guard is gone: `Drop` takes the same
        // lock, and a claim built eagerly and discarded here would deadlock.
        drop(claimed);
        taken.then(|| Self { path })
    }
}

impl Drop for PathClaim {
    fn drop(&mut self) {
        if let Ok(mut claimed) = claimed_paths().lock() {
            claimed.remove(&self.path);
        }
    }
}

/// The lock path in a form two callers spell the same way, without opening it.
///
/// Only the directory is canonicalized: the lock file may not exist yet, and
/// creating it here would defeat the point of claiming before opening.
fn normalized(lock_path: &Path) -> PathBuf {
    match (lock_path.parent(), lock_path.file_name()) {
        (Some(parent), Some(name)) => fs::canonicalize(parent)
            .map_or_else(|_| lock_path.to_owned(), |parent| parent.join(name)),
        _ => lock_path.to_owned(),
    }
}

/// Whether this process took the whole-file write lock; `false` if another
/// process holds it.
#[cfg(unix)]
fn take_record_lock(file: &File) -> io::Result<bool> {
    use std::os::fd::AsRawFd;

    let mut lock: libc::flock = unsafe { std::mem::zeroed() };
    lock.l_type = libc::F_WRLCK as libc::c_short;
    lock.l_whence = libc::SEEK_SET as libc::c_short;
    lock.l_start = 0;
    lock.l_len = 0;
    // SAFETY: `lock` is a fully initialized `flock` and the descriptor is open
    // for writing for the duration of the call.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETLK, &mut lock) } != -1 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(code) if code == libc::EACCES || code == libc::EAGAIN => Ok(false),
        _ => Err(error),
    }
}

#[cfg(not(unix))]
fn take_record_lock(file: &File) -> io::Result<bool> {
    use fs2::FileExt;
    match FileExt::try_lock_exclusive(file) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(false),
        Err(error) => Err(error),
    }
}

/// Whether some writer owns this journal right now, without disturbing it.
///
/// The registry is asked first, and not only for speed: `F_GETLK` reports
/// conflicts with *other* processes and says nothing about our own locks, and
/// opening the lock file to ask would, on the way back out, drop every record
/// lock this process holds on it. A journal we own is answered from memory and
/// the file is never touched.
pub fn writer_present(journal_path: &Path) -> io::Result<bool> {
    let lock_path = sibling_path(journal_path, ".lock");
    if let Ok(claimed) = claimed_paths().lock()
        && claimed.contains(&normalized(&lock_path))
    {
        return Ok(true);
    }
    let file = match OpenOptions::new().read(true).write(true).open(&lock_path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    record_lock_holder(&file)
}

#[cfg(unix)]
fn record_lock_holder(file: &File) -> io::Result<bool> {
    use std::os::fd::AsRawFd;

    let mut probe: libc::flock = unsafe { std::mem::zeroed() };
    probe.l_type = libc::F_WRLCK as libc::c_short;
    probe.l_whence = libc::SEEK_SET as libc::c_short;
    probe.l_start = 0;
    probe.l_len = 0;
    // SAFETY: `probe` is a fully initialized `flock` and the descriptor is open
    // for the duration of the call.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETLK, &mut probe) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(probe.l_type != libc::F_UNLCK as libc::c_short)
}

#[cfg(not(unix))]
fn record_lock_holder(file: &File) -> io::Result<bool> {
    use fs2::FileExt;
    match FileExt::try_lock_exclusive(file) {
        Ok(()) => {
            let _ = FileExt::unlock(file);
            Ok(false)
        }
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(true),
        Err(error) => Err(error),
    }
}

pub struct Journal {
    source_id: SourceId,
    path: PathBuf,
    seq_path: PathBuf,
    file: File,
    _lock: File,
    _claim: PathClaim,
    next_sequence: u64,
    reserved_until: u64,
    poisoned: bool,
}

impl Drop for Journal {
    fn drop(&mut self) {
        trace::record(|| {
            format!(
                "journal DROPPED source={:?} fd={} thread={:?}",
                self.source_id,
                std::os::fd::AsRawFd::as_raw_fd(&self._lock),
                std::thread::current().id()
            )
        });
    }
}

impl Journal {
    pub fn open(
        path: impl AsRef<Path>,
        source_id: SourceId,
    ) -> Result<(Self, Recovery), JournalError> {
        let path = path.as_ref().to_owned();
        let seq_path = sibling_path(&path, ".seq");
        let lock_path = sibling_path(&path, ".lock");
        // Before the file is opened, so that a refusal cannot close a
        // descriptor an incumbent journal's record lock depends on.
        let Some(claim) = PathClaim::take(&lock_path) else {
            return Err(refuse(source_id, &lock_path, None));
        };
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)?;
        if !take_record_lock(&lock)? {
            return Err(refuse(source_id, &lock_path, Some(&lock)));
        }
        trace::record(|| {
            format!(
                "open OK source={source_id:?} fd={} thread={:?}",
                std::os::fd::AsRawFd::as_raw_fd(&lock),
                std::thread::current().id()
            )
        });
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .append(true)
            .open(&path)?;
        let scan = scan_metadata(&mut file, source_id)?;
        let actual = file.metadata()?.len();
        let truncated_tail_bytes = actual - scan.valid_len;
        if truncated_tail_bytes > 0 {
            file.set_len(scan.valid_len)?;
            file.sync_data()?;
        }
        let watermark = read_watermark(&seq_path)?.unwrap_or(0);
        let after_records = scan
            .maximum_sequence
            .map_or(0, |value| value.saturating_add(1));
        let next_sequence = watermark.max(after_records);
        file.seek(SeekFrom::End(0))?;
        Ok((
            Self {
                source_id,
                path,
                seq_path,
                file,
                _lock: lock,
                _claim: claim,
                next_sequence,
                reserved_until: next_sequence,
                poisoned: false,
            },
            Recovery {
                records: scan.records,
                truncated_tail_bytes,
                next_sequence,
                last_sequence: scan.maximum_sequence,
            },
        ))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn next_sequence(&self) -> u64 {
        self.next_sequence
    }
    pub fn end_offset(&self) -> Result<u64, JournalError> {
        Ok(self.file.metadata()?.len())
    }

    pub fn append(&mut self, mut record: RawRecord) -> Result<RecordId, JournalError> {
        if self.poisoned {
            return Err(JournalError::Poisoned);
        }
        if record.record_id.source_id != self.source_id {
            return Err(JournalError::WrongSource);
        }
        let sequence = self.next_sequence;
        let next = match sequence.checked_add(1) {
            Some(next) => next,
            None => {
                let offset = self.file.stream_position()?;
                return Err(JournalError::Corrupt {
                    offset,
                    reason: "sequence exhausted",
                });
            }
        };
        record.record_id.sequence = sequence;
        let body = encode(&record)?;
        let frame = encode_frame(&body);
        if next > self.reserved_until {
            let reserved_until = match sequence.checked_add(SEQUENCE_BLOCK) {
                Some(limit) => limit,
                None => {
                    let offset = self.file.stream_position()?;
                    return Err(JournalError::Corrupt {
                        offset,
                        reason: "sequence reservation exhausted",
                    });
                }
            };
            write_watermark(&self.seq_path, reserved_until)?;
            self.reserved_until = reserved_until;
        }
        if let Err(error) = self.file.write_all(&frame) {
            self.poisoned = true;
            return Err(JournalError::Io(error));
        }
        self.next_sequence = next;
        Ok(record.record_id)
    }

    pub fn flush(&mut self) -> Result<(), JournalError> {
        if let Err(error) = self.file.flush() {
            self.poisoned = true;
            return Err(JournalError::Io(error));
        }
        Ok(())
    }
    pub fn sync_data(&mut self) -> Result<(), JournalError> {
        if let Err(error) = self.file.flush().and_then(|()| self.file.sync_data()) {
            self.poisoned = true;
            return Err(JournalError::Io(error));
        }
        Ok(())
    }

    /// Reads a bounded page. Offset must be zero or a previous page's `next_offset`.
    ///
    /// The byte limit is soft for the first record: one record of at most
    /// `MAX_FRAME` decoded bytes is returned to guarantee forward progress.
    pub fn read_page(
        &mut self,
        offset: u64,
        max_records: usize,
        max_bytes: usize,
    ) -> Result<JournalPage, JournalError> {
        read_page_from(
            &mut self.file,
            self.source_id,
            offset,
            max_records,
            max_bytes,
        )
    }

    /// Explicit whole-history convenience for tests and exports; UI code should page.
    pub fn collect_all_records(&mut self) -> Result<Vec<RawRecord>, JournalError> {
        let mut offset = 0;
        let mut records = Vec::new();
        loop {
            let page = self.read_page(offset, 1024, 8 * 1024 * 1024)?;
            records.extend(page.records);
            if page.end_of_journal {
                return Ok(records);
            }
            offset = page.next_offset;
        }
    }
}

fn read_page_from(
    file: &mut File,
    source_id: SourceId,
    offset: u64,
    max_records: usize,
    max_bytes: usize,
) -> Result<JournalPage, JournalError> {
    let total = file.metadata()?.len();
    if offset > total {
        return Err(JournalError::Corrupt {
            offset,
            reason: "page offset beyond journal",
        });
    }
    if max_records == 0 || max_bytes == 0 {
        return Ok(JournalPage {
            records: Vec::new(),
            next_offset: offset,
            end_of_journal: offset == total,
        });
    }
    file.seek(SeekFrom::Start(offset))?;
    // Bound read-ahead per page. The writer remains append-only and every
    // subsequent page seeks explicitly, so discarded read-ahead cannot change
    // publication offsets or reposition an append over existing records.
    let mut reader = BufReader::with_capacity(64 * 1024, file);
    let mut cursor = offset;
    let mut decoded_bytes = 0usize;
    let mut records = Vec::new();
    while cursor < total && records.len() < max_records {
        let (record, frame_len) = read_complete_frame(&mut reader, cursor, total, source_id)?;
        let record_bytes = record.bytes.len() + record.delimiter.len();
        if !records.is_empty() && decoded_bytes.saturating_add(record_bytes) > max_bytes {
            reader.seek(SeekFrom::Start(cursor))?;
            break;
        }
        decoded_bytes = decoded_bytes.saturating_add(record_bytes);
        records.push(record);
        cursor += frame_len;
        if decoded_bytes >= max_bytes {
            break;
        }
    }
    Ok(JournalPage {
        records,
        next_offset: cursor,
        end_of_journal: cursor == total,
    })
}

fn sibling_path(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_owned();
    value.push(suffix);
    PathBuf::from(value)
}

fn write_watermark(path: &Path, value: u64) -> io::Result<()> {
    let tmp = sibling_path(path, ".tmp");
    {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&tmp)?;
        file.write_all(&value.to_le_bytes())?;
        file.sync_data()?;
    }
    fs::rename(tmp, path)?;
    let parent = path
        .parent()
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn read_watermark(path: &Path) -> io::Result<Option<u64>> {
    match fs::read(path) {
        Ok(value) if value.len() == 8 => Ok(Some(u64::from_le_bytes(value.try_into().unwrap()))),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid sequence watermark",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn encode_frame(body: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(HEADER + body.len());
    frame.extend(MAGIC);
    frame.extend((body.len() as u32).to_le_bytes());
    frame.extend(hash(body).to_le_bytes());
    frame.extend(hash(&frame).to_le_bytes());
    frame.extend(body);
    frame
}

fn encode(record: &RawRecord) -> Result<Vec<u8>, JournalError> {
    let length = FIXED + record.bytes.len() + record.delimiter.len();
    if length > MAX_FRAME {
        return Err(JournalError::FrameTooLarge);
    }
    let mut body = Vec::with_capacity(length);
    body.extend(record.record_id.sequence.to_le_bytes());
    body.extend(record.record_id.source_id.0.as_bytes());
    body.extend(record.captured_at_unix_nanos.to_le_bytes());
    body.push(match record.stream {
        StreamKind::Stdout => 0,
        StreamKind::Stderr => 1,
        StreamKind::File => 2,
        StreamKind::Http => 3,
        StreamKind::Stdin => 4,
    });
    body.extend(record.acquisition_id.as_bytes());
    body.push(match record.chunk {
        ChunkPosition::Complete => 0,
        ChunkPosition::Start => 1,
        ChunkPosition::Continue => 2,
        ChunkPosition::End => 3,
    });
    body.extend((record.bytes.len() as u32).to_le_bytes());
    body.extend((record.delimiter.len() as u32).to_le_bytes());
    body.extend(&record.bytes);
    body.extend(&record.delimiter);
    Ok(body)
}

fn scan_metadata(file: &mut File, source: SourceId) -> Result<ScanMetadata, JournalError> {
    file.seek(SeekFrom::Start(0))?;
    let total = file.metadata()?.len();
    let mut offset = 0;
    let mut records = 0;
    let mut maximum_sequence = None;
    while offset < total {
        if total - offset < HEADER as u64 {
            break;
        }
        let header = read_header(file, offset)?;
        let frame_len = HEADER as u64 + header.length as u64;
        if total - offset < frame_len {
            break;
        }
        let (record, _) = read_body(file, offset, source, header)?;
        if let Some(last) = maximum_sequence
            && record.record_id.sequence <= last
        {
            return Err(JournalError::Corrupt {
                offset,
                reason: "non-monotonic sequence",
            });
        }
        maximum_sequence = Some(record.record_id.sequence);
        records += 1;
        offset += frame_len;
    }
    Ok(ScanMetadata {
        records,
        valid_len: offset,
        maximum_sequence,
    })
}

#[derive(Clone, Copy)]
struct FrameHeader {
    length: usize,
    body_checksum: u32,
}

fn read_header(file: &mut impl Read, offset: u64) -> Result<FrameHeader, JournalError> {
    let mut bytes = [0; HEADER];
    file.read_exact(&mut bytes)?;
    if &bytes[..4] != MAGIC {
        return Err(JournalError::Corrupt {
            offset,
            reason: "bad frame magic",
        });
    }
    if hash(&bytes[..12]) != u32::from_le_bytes(bytes[12..16].try_into().unwrap()) {
        return Err(JournalError::Corrupt {
            offset,
            reason: "header checksum mismatch",
        });
    }
    let length = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
    if !(FIXED..=MAX_FRAME).contains(&length) {
        return Err(JournalError::Corrupt {
            offset,
            reason: "invalid frame length",
        });
    }
    Ok(FrameHeader {
        length,
        body_checksum: u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
    })
}

fn read_body(
    file: &mut impl Read,
    offset: u64,
    source: SourceId,
    header: FrameHeader,
) -> Result<(RawRecord, u64), JournalError> {
    let mut body = vec![0; header.length];
    file.read_exact(&mut body)?;
    if hash(&body) != header.body_checksum {
        return Err(JournalError::Corrupt {
            offset,
            reason: "body checksum mismatch",
        });
    }
    let record = decode(&body, source).ok_or(JournalError::Corrupt {
        offset,
        reason: "invalid frame body",
    })?;
    Ok((record, (HEADER + header.length) as u64))
}

fn read_complete_frame(
    file: &mut impl Read,
    offset: u64,
    total: u64,
    source: SourceId,
) -> Result<(RawRecord, u64), JournalError> {
    if total - offset < HEADER as u64 {
        return Err(JournalError::Corrupt {
            offset,
            reason: "page begins at incomplete frame",
        });
    }
    let header = read_header(file, offset)?;
    if total - offset < (HEADER + header.length) as u64 {
        return Err(JournalError::Corrupt {
            offset,
            reason: "page encounters incomplete frame",
        });
    }
    read_body(file, offset, source, header)
}

fn decode(body: &[u8], source: SourceId) -> Option<RawRecord> {
    let sequence = u64::from_le_bytes(body[0..8].try_into().ok()?);
    if Uuid::from_slice(&body[8..24]).ok()? != source.0 {
        return None;
    }
    let captured_at_unix_nanos = i64::from_le_bytes(body[24..32].try_into().ok()?);
    let stream = match body[32] {
        0 => StreamKind::Stdout,
        1 => StreamKind::Stderr,
        2 => StreamKind::File,
        3 => StreamKind::Http,
        4 => StreamKind::Stdin,
        _ => return None,
    };
    let acquisition_id = Uuid::from_slice(&body[33..49]).ok()?;
    let chunk = match body[49] {
        0 => ChunkPosition::Complete,
        1 => ChunkPosition::Start,
        2 => ChunkPosition::Continue,
        3 => ChunkPosition::End,
        _ => return None,
    };
    let bytes_len = u32::from_le_bytes(body[50..54].try_into().ok()?) as usize;
    let delimiter_len = u32::from_le_bytes(body[54..58].try_into().ok()?) as usize;
    if FIXED + bytes_len + delimiter_len != body.len() {
        return None;
    }
    Some(RawRecord {
        record_id: RecordId {
            source_id: source,
            sequence,
        },
        captured_at_unix_nanos,
        stream,
        bytes: body[58..58 + bytes_len].to_vec(),
        delimiter: body[58 + bytes_len..].to_vec(),
        acquisition_id,
        chunk,
    })
}

/// Records why an open was refused and returns the error to report.
fn refuse(source_id: SourceId, lock_path: &Path, refused: Option<&File>) -> JournalError {
    if trace::enabled() {
        trace::record(|| {
            format!(
                "open REFUSED source={source_id:?} thread={:?}",
                std::thread::current().id()
            )
        });
        let refused_fd = refused.map_or(-1, std::os::fd::AsRawFd::as_raw_fd);
        trace::record(|| holder_report(lock_path, refused_fd));
        trace::dump("journal already open");
    }
    JournalError::AlreadyOpen
}

/// Who the kernel says holds the lock on this file, and which of our own
/// descriptors still point at it.
fn holder_report(lock_path: &Path, refused_fd: std::os::fd::RawFd) -> String {
    use std::os::unix::fs::MetadataExt;
    let Ok(meta) = std::fs::metadata(lock_path) else {
        return "holder: lock file is gone".to_owned();
    };
    let (device, inode) = (meta.dev(), meta.ino());
    let major = (device >> 8) & 0xfff;
    let minor = (device & 0xff) | ((device >> 12) & 0xfff00);
    let want = format!("{major:02x}:{minor:02x}:{inode}");
    let mut holders = Vec::new();
    if let Ok(locks) = std::fs::read_to_string("/proc/locks") {
        for line in locks.lines() {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() >= 6 && fields[5] == want {
                holders.push(format!(
                    "pid {} kind {} {}",
                    fields[4], fields[1], fields[3]
                ));
            }
        }
    }
    let mut own = Vec::new();
    if let Ok(target) = std::fs::canonicalize(lock_path)
        && let Ok(fds) = std::fs::read_dir("/proc/self/fd")
    {
        for fd in fds.flatten() {
            if std::fs::read_link(fd.path()).is_ok_and(|p| p == target) {
                own.push(format!("{:?}", fd.file_name()));
            }
        }
    }
    format!(
        "holder: self={} inode={want} refused_fd={refused_fd} \
         kernel_holders={holders:?} own_fds={own:?}",
        std::process::id()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn record(source_id: SourceId) -> RawRecord {
        RawRecord {
            record_id: RecordId {
                source_id,
                sequence: 0,
            },
            captured_at_unix_nanos: 0,
            stream: StreamKind::File,
            bytes: b"recoverable".to_vec(),
            delimiter: b"\n".to_vec(),
            acquisition_id: Uuid::new_v4(),
            chunk: ChunkPosition::Complete,
        }
    }

    #[test]
    fn append_io_failure_poisons_until_recovery_reopen() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("poison.lvu");
        let source = SourceId::new();
        let (mut journal, _) = Journal::open(&path, source).unwrap();
        journal.file = File::open(directory.path()).unwrap();
        assert!(matches!(
            journal.append(record(source)),
            Err(JournalError::Io(_))
        ));
        assert!(matches!(
            journal.append(record(source)),
            Err(JournalError::Poisoned)
        ));
        drop(journal);

        let (mut recovered, recovery) = Journal::open(&path, source).unwrap();
        assert_eq!(recovery.records, 0);
        assert_eq!(
            recovered.append(record(source)).unwrap().sequence,
            SEQUENCE_BLOCK
        );
    }

    #[test]
    fn stdin_extends_stream_encoding_without_renumbering_existing_kinds() {
        let source = SourceId::new();
        for (stream, discriminant) in [
            (StreamKind::Stdout, 0),
            (StreamKind::Stderr, 1),
            (StreamKind::File, 2),
            (StreamKind::Http, 3),
            (StreamKind::Stdin, 4),
        ] {
            let mut value = record(source);
            value.stream = stream;
            let body = encode(&value).unwrap();
            assert_eq!(body[32], discriminant);
            assert_eq!(decode(&body, source).unwrap().stream, stream);
        }
    }
}
