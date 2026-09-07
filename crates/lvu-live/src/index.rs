use crc32fast::hash;
use fs2::FileExt;
use lvu_core::{RawRecord, SourceId};
use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

const MAGIC: &[u8; 8] = b"LVUIDX3\0";
const OLD_MAGIC: &[u8; 8] = b"LVUIDX2\0";
const HEADER_LEN: u64 = 60;
const OLD_HEADER_LEN: u64 = 44;
const ENTRY_LEN: u64 = 40;
const BUDGET_MAGIC: &[u8; 8] = b"LVUBGT1\0";
const BUDGET_LEN: usize = 32;
const BUDGET_FILE: &str = ".lvu-index-budget";

#[derive(Clone, Copy)]
pub(crate) struct IndexBudget {
    pub per_source: u64,
    pub total: u64,
    pub reconciliation_limit: usize,
}

pub(crate) fn validate_owned_artifact(
    file: &mut File,
    source: &[u8; 16],
    maximum_bytes: u64,
    cancelled: impl Fn() -> bool,
) -> io::Result<()> {
    let length = file.metadata()?.len();
    file.seek(SeekFrom::Start(0))?;
    let mut magic = [0; 8];
    file.read_exact(&mut magic)?;
    let header_len = if &magic == MAGIC {
        HEADER_LEN
    } else if &magic == OLD_MAGIC {
        OLD_HEADER_LEN
    } else {
        return Err(invalid("index ownership header mismatch"));
    };
    if length > maximum_bytes
        || length < header_len
        || !(length - header_len).is_multiple_of(ENTRY_LEN)
    {
        return Err(invalid("invalid index length"));
    }
    file.seek(SeekFrom::Start(0))?;
    let mut header = vec![0u8; header_len as usize];
    file.read_exact(&mut header)?;
    let checksum_at = header.len() - 4;
    if &header[8..24] != source
        || u32::from_le_bytes(header[checksum_at..].try_into().expect("fixed slice"))
            != hash(&header[..checksum_at])
    {
        return Err(invalid("index ownership header mismatch"));
    }
    let count = (length - header_len) / ENTRY_LEN;
    let mut previous = None;
    for position in 0..count {
        if position.is_multiple_of(1024) && cancelled() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "index validation cancelled",
            ));
        }
        let mut bytes = [0u8; ENTRY_LEN as usize];
        file.read_exact(&mut bytes)?;
        let entry = decode_entry(&bytes)?;
        if entry.position != position || previous.is_some_and(|value| value >= entry.sequence) {
            return Err(invalid("invalid index ownership entries"));
        }
        previous = Some(entry.sequence);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct IndexEntry {
    pub sequence: u64,
    pub position: u64,
    pub page_offset: u64,
    pub next_offset: u64,
    pub within_page: u32,
}

pub(crate) struct DiskIndex {
    file: File,
    path: PathBuf,
    maximum_total_bytes: u64,
    reconciliation_limit: usize,
    pub count: u64,
    pub next_offset: u64,
    pub high_sequence: Option<u64>,
}

impl DiskIndex {
    #[cfg(test)]
    pub fn open(
        path: &Path,
        source: SourceId,
        generation: u64,
        page_records: usize,
        page_bytes: usize,
        maximum_bytes: u64,
    ) -> io::Result<(Self, bool)> {
        Self::open_budgeted(
            path,
            source,
            generation,
            page_records,
            page_bytes,
            [0; 16],
            IndexBudget {
                per_source: maximum_bytes,
                total: u64::MAX,
                reconciliation_limit: 4096,
            },
        )
        .map(|(index, rebuilt, _)| (index, rebuilt))
    }

    pub fn open_budgeted(
        path: &Path,
        source: SourceId,
        generation: u64,
        page_records: usize,
        page_bytes: usize,
        journal_identity: [u8; 16],
        budget: IndexBudget,
    ) -> io::Result<(Self, bool, BudgetVerification)> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let _ownership = ownership_lock(path)?;
        let directory = path
            .parent()
            .ok_or_else(|| invalid("index has no parent"))?;
        let accounted = reconcile_budget(directory, budget.reconciliation_limit, budget.total)?;
        let verification = if accounted.verified {
            BudgetVerification::Verified
        } else {
            BudgetVerification::Unverified
        };
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)?;
        file.try_lock_exclusive().map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("derived index is already owned: {error}"),
            )
        })?;
        let rebuilt = match validate(
            &mut file,
            source,
            generation,
            page_records,
            page_bytes,
            journal_identity,
            budget.per_source,
        ) {
            Ok(metadata) => {
                return Ok((
                    Self {
                        file,
                        path: path.to_path_buf(),
                        maximum_total_bytes: budget.total,
                        reconciliation_limit: budget.reconciliation_limit,
                        count: metadata.0,
                        next_offset: metadata.1,
                        high_sequence: metadata.2,
                    },
                    false,
                    verification,
                ));
            }
            Err(_) => true,
        };
        let prior_length = file.metadata()?.len();
        let resized_total = accounted
            .bytes
            .saturating_sub(prior_length)
            .saturating_add(HEADER_LEN);
        // An unverified aggregate is a number we could not confirm, not a limit
        // we know was reached. Refusing here meant one directory holding more
        // stale indexes than the reconciliation bound stopped an unrelated new
        // source from showing a single row. The per-source cap is still exact
        // and still enforced, so growth stays bounded; the caller reports the
        // aggregate as unverified instead of pretending it is exhausted.
        if accounted.verified && resized_total > budget.total {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                format!(
                    "global derived-index budget reached: header would require {resized_total} of {} bytes",
                    budget.total
                ),
            ));
        }
        let positive_delta = HEADER_LEN.saturating_sub(prior_length);
        reserve_budget(path, positive_delta, budget.total)?;
        let mutation = (|| {
            file.set_len(0)?;
            maybe_fail(path, FaultPoint::RebuildWrite)?;
            file.seek(SeekFrom::Start(0))?;
            file.write_all(&header(
                source,
                generation,
                page_records,
                page_bytes,
                journal_identity,
            )?)?;
            maybe_fail(path, FaultPoint::RebuildFlush)?;
            file.flush()
        })();
        if let Err(error) = mutation {
            reconcile_after_mutation(path, budget);
            return Err(error);
        }
        reconcile_budget(directory, budget.reconciliation_limit, budget.total)?;
        Ok((
            Self {
                file,
                path: path.to_path_buf(),
                maximum_total_bytes: budget.total,
                reconciliation_limit: budget.reconciliation_limit,
                count: 0,
                next_offset: 0,
                high_sequence: None,
            },
            rebuilt,
            verification,
        ))
    }

    pub fn append_page(
        &mut self,
        page_offset: u64,
        next_offset: u64,
        records: &[RawRecord],
        maximum_bytes: u64,
    ) -> io::Result<()> {
        let added = (records.len() as u64).saturating_mul(ENTRY_LEN);
        if self.file.metadata()?.len().saturating_add(added) > maximum_bytes {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "derived index byte limit reached",
            ));
        }
        if records.is_empty() {
            return Ok(());
        }
        let original_length = self.file.seek(SeekFrom::End(0))?;
        let original_count = self.count;
        let final_position = original_count + records.len() as u64 - 1;
        let committed = IndexEntry {
            sequence: records.last().expect("nonempty page").record_id.sequence,
            position: final_position,
            page_offset,
            next_offset,
            within_page: (records.len() - 1).try_into().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "page position exceeds u32")
            })?,
        };
        let _ownership = ownership_lock(&self.path)?;
        reserve_budget(&self.path, added, self.maximum_total_bytes)?;
        let mutation = (|| {
            for (within, record) in records.iter().enumerate() {
                let entry = IndexEntry {
                    sequence: record.record_id.sequence,
                    position: original_count + within as u64,
                    page_offset,
                    // A page is committed only by rewriting its final entry below.
                    // A crash before that point leaves a recoverable provisional suffix.
                    next_offset: page_offset,
                    within_page: within.try_into().map_err(|_| {
                        io::Error::new(io::ErrorKind::InvalidData, "page position exceeds u32")
                    })?,
                };
                self.file.write_all(&encode_entry(entry))?;
                maybe_fail(&self.path, FaultPoint::EntryWrite)?;
            }
            maybe_fail(&self.path, FaultPoint::EntryFlush)?;
            self.file.flush()?;
            self.file.seek(SeekFrom::Start(
                original_length + (records.len() as u64 - 1) * ENTRY_LEN,
            ))?;
            self.file.write_all(&encode_entry(committed))?;
            maybe_fail(&self.path, FaultPoint::CommitFlush)?;
            self.file.flush()
        })();
        if let Err(error) = mutation {
            let _ = self
                .file
                .set_len(original_length)
                .and_then(|()| self.file.flush());
            reconcile_after_mutation(
                &self.path,
                IndexBudget {
                    per_source: maximum_bytes,
                    total: self.maximum_total_bytes,
                    reconciliation_limit: self.reconciliation_limit,
                },
            );
            return Err(error);
        }
        self.count = original_count + records.len() as u64;
        self.high_sequence = Some(committed.sequence);
        self.next_offset = next_offset;
        Ok(())
    }

    pub fn entries(&mut self, start: u64, len: usize) -> io::Result<Vec<IndexEntry>> {
        if start >= self.count || len == 0 {
            return Ok(Vec::new());
        }
        let available = self.count.saturating_sub(start);
        let count = usize::try_from(available.min(len as u64)).unwrap_or(len);
        self.file
            .seek(SeekFrom::Start(HEADER_LEN + start * ENTRY_LEN))?;
        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            let mut bytes = [0u8; ENTRY_LEN as usize];
            self.file.read_exact(&mut bytes)?;
            entries.push(decode_entry(&bytes)?);
        }
        Ok(entries)
    }

    pub fn find_sequence(&mut self, sequence: u64) -> io::Result<Option<IndexEntry>> {
        let mut low = 0u64;
        let mut high = self.count;
        while low < high {
            let middle = low + (high - low) / 2;
            let entry = self.entry(middle)?;
            match entry.sequence.cmp(&sequence) {
                std::cmp::Ordering::Less => low = middle + 1,
                std::cmp::Ordering::Greater => high = middle,
                std::cmp::Ordering::Equal => return Ok(Some(entry)),
            }
        }
        Ok(None)
    }

    fn entry(&mut self, position: u64) -> io::Result<IndexEntry> {
        self.file
            .seek(SeekFrom::Start(HEADER_LEN + position * ENTRY_LEN))?;
        let mut bytes = [0u8; ENTRY_LEN as usize];
        self.file.read_exact(&mut bytes)?;
        decode_entry(&bytes)
    }
}

fn ownership_lock(path: &Path) -> io::Result<File> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid("index has no parent directory"))?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(parent.join(".lvu-index-ownership.lock"))?;
    lock.lock_exclusive()?;
    Ok(lock)
}

/// Whether the shared on-disk index total could be accounted for.
///
/// Bounded reconciliation stops after a fixed number of directory entries, so a
/// cache holding more indexes than that bound leaves the aggregate unknown.
/// Unknown is not the same as exhausted, and only the former is recoverable by
/// cleaning the directory, so the two are reported separately.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BudgetVerification {
    Verified,
    Unverified,
}

#[derive(Clone, Copy)]
struct BudgetState {
    bytes: u64,
    maximum: u64,
    verified: bool,
}

fn reconcile_budget(
    directory: &Path,
    limit: usize,
    requested_maximum: u64,
) -> io::Result<BudgetState> {
    let mut bytes = 0u64;
    let mut verified = true;
    let mut active = false;
    for (seen, entry) in std::fs::read_dir(directory)?.enumerate() {
        if seen >= limit {
            verified = false;
            break;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                verified = false;
                continue;
            }
        };
        let path = entry.path();
        if !path
            .file_name()
            .is_some_and(|value| value.to_string_lossy().ends_with(".rows.idx"))
        {
            continue;
        }
        match entry.file_type() {
            Ok(kind) if kind.is_file() => match entry.metadata() {
                Ok(metadata) => {
                    bytes = bytes.saturating_add(metadata.len());
                    match OpenOptions::new().read(true).write(true).open(entry.path()) {
                        Ok(file) => match file.try_lock_exclusive() {
                            Ok(()) => drop(file),
                            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                                active = true
                            }
                            Err(_) => verified = false,
                        },
                        Err(_) => verified = false,
                    }
                }
                Err(_) => verified = false,
            },
            _ => verified = false,
        }
    }
    let existing = match read_budget(directory) {
        Ok(state) => Some(state),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(_) if active => {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "global derived-index budget ledger is unverified while writers are active; growth refused",
            ));
        }
        Err(_) => None,
    };
    if let Some(existing) = existing
        && existing.maximum != requested_maximum
        && active
    {
        return Err(io::Error::new(
            io::ErrorKind::WriteZero,
            format!(
                "global derived-index budget mismatch: active providers use {} bytes, requested {} bytes",
                existing.maximum, requested_maximum
            ),
        ));
    }
    let state = BudgetState {
        bytes,
        maximum: requested_maximum,
        verified,
    };
    write_budget(directory, state)?;
    Ok(state)
}

fn reserve_budget(path: &Path, added: u64, maximum: u64) -> io::Result<()> {
    let directory = path
        .parent()
        .ok_or_else(|| invalid("index has no parent"))?;
    let mut state = read_budget(directory)?;
    if !state.verified {
        // The aggregate is unknown, so enforcing it would be guesswork in both
        // directions. Growth is still bounded by this index's own cap, checked
        // by the caller against a size we do know exactly. Keep the ledger
        // marked unverified so a later clean reconciliation is what restores
        // aggregate enforcement.
        state.bytes = state.bytes.saturating_add(added);
        write_budget(directory, state)?;
        return Ok(());
    }
    if state.maximum != maximum {
        return Err(io::Error::new(
            io::ErrorKind::WriteZero,
            format!(
                "global derived-index budget mismatch: shared cap is {} bytes, provider requested {} bytes",
                state.maximum, maximum
            ),
        ));
    }
    if state.bytes.saturating_add(added) > state.maximum {
        return Err(io::Error::new(
            io::ErrorKind::WriteZero,
            format!(
                "global derived-index budget reached: {} + {} exceeds {} bytes",
                state.bytes, added, state.maximum
            ),
        ));
    }
    state.bytes += added;
    write_budget(directory, state)
}

fn release_budget_locked(path: &Path, released: u64) -> io::Result<()> {
    let directory = path
        .parent()
        .ok_or_else(|| invalid("index has no parent"))?;
    let mut state = read_budget(directory)?;
    state.bytes = state.bytes.saturating_sub(released);
    write_budget(directory, state)
}

fn reconcile_after_mutation(path: &Path, budget: IndexBudget) {
    let Some(directory) = path.parent() else {
        return;
    };
    if reconcile_budget(directory, budget.reconciliation_limit, budget.total).is_err()
        && let Ok(mut state) = read_budget(directory)
    {
        // The prior reservation remains included. Marking it unverified is
        // conservative: no writer may grow until a later clean reconciliation.
        state.verified = false;
        let _ = write_budget(directory, state);
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum FaultPoint {
    RebuildWrite,
    RebuildFlush,
    EntryWrite,
    EntryFlush,
    CommitFlush,
}

#[cfg(not(test))]
fn maybe_fail(_path: &Path, _point: FaultPoint) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
static INJECTED_FAILURE: std::sync::Mutex<Option<(PathBuf, FaultPoint)>> =
    std::sync::Mutex::new(None);

#[cfg(test)]
fn maybe_fail(path: &Path, point: FaultPoint) -> io::Result<()> {
    let mut failure = INJECTED_FAILURE.lock().expect("fault injection poisoned");
    if failure
        .as_ref()
        .is_some_and(|(target, expected)| target == path && *expected == point)
    {
        failure.take();
        Err(io::Error::other("injected derived-index write failure"))
    } else {
        Ok(())
    }
}

pub(crate) fn release_global_budget_locked(path: &Path, released: u64) -> io::Result<()> {
    match release_budget_locked(path, released) {
        Ok(()) => Ok(()),
        Err(_) => {
            let directory = path
                .parent()
                .ok_or_else(|| invalid("index has no parent"))?;
            let maximum = read_budget(directory).map_or(u64::MAX, |state| state.maximum);
            reconcile_budget(directory, 4096, maximum).map(|_| ())
        }
    }
}

fn read_budget(directory: &Path) -> io::Result<BudgetState> {
    let path = directory.join(BUDGET_FILE);
    let mut file = OpenOptions::new().read(true).open(path)?;
    let mut bytes = [0u8; BUDGET_LEN];
    file.read_exact(&mut bytes)?;
    if &bytes[..8] != BUDGET_MAGIC
        || u32::from_le_bytes(bytes[28..32].try_into().expect("fixed slice")) != hash(&bytes[..28])
    {
        return Err(invalid("global derived-index budget ledger is invalid"));
    }
    Ok(BudgetState {
        bytes: u64::from_le_bytes(bytes[8..16].try_into().expect("fixed slice")),
        maximum: u64::from_le_bytes(bytes[16..24].try_into().expect("fixed slice")),
        verified: bytes[24] == 1,
    })
}

fn write_budget(directory: &Path, state: BudgetState) -> io::Result<()> {
    let mut bytes = [0u8; BUDGET_LEN];
    bytes[..8].copy_from_slice(BUDGET_MAGIC);
    bytes[8..16].copy_from_slice(&state.bytes.to_le_bytes());
    bytes[16..24].copy_from_slice(&state.maximum.to_le_bytes());
    bytes[24] = u8::from(state.verified);
    let checksum = hash(&bytes[..28]);
    bytes[28..32].copy_from_slice(&checksum.to_le_bytes());
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(directory.join(BUDGET_FILE))?;
    file.write_all(&bytes)?;
    file.flush()
}

fn header(
    source: SourceId,
    generation: u64,
    page_records: usize,
    page_bytes: usize,
    journal_identity: [u8; 16],
) -> io::Result<[u8; HEADER_LEN as usize]> {
    let mut bytes = [0u8; HEADER_LEN as usize];
    bytes[..8].copy_from_slice(MAGIC);
    bytes[8..24].copy_from_slice(source.0.as_bytes());
    bytes[24..32].copy_from_slice(&generation.to_le_bytes());
    bytes[32..36].copy_from_slice(
        &u32::try_from(page_records)
            .map_err(|_| invalid("index page record bound exceeds format"))?
            .to_le_bytes(),
    );
    bytes[36..40].copy_from_slice(
        &u32::try_from(page_bytes)
            .map_err(|_| invalid("index page byte bound exceeds format"))?
            .to_le_bytes(),
    );
    bytes[40..56].copy_from_slice(&journal_identity);
    let checksum = hash(&bytes[..56]);
    bytes[56..60].copy_from_slice(&checksum.to_le_bytes());
    Ok(bytes)
}

fn validate(
    file: &mut File,
    source: SourceId,
    generation: u64,
    page_records: usize,
    page_bytes: usize,
    journal_identity: [u8; 16],
    maximum_bytes: u64,
) -> io::Result<(u64, u64, Option<u64>)> {
    let length = file.metadata()?.len();
    if length > maximum_bytes
        || length < HEADER_LEN
        || !(length - HEADER_LEN).is_multiple_of(ENTRY_LEN)
    {
        return Err(invalid("invalid index length"));
    }
    file.seek(SeekFrom::Start(0))?;
    let mut header_bytes = [0u8; HEADER_LEN as usize];
    file.read_exact(&mut header_bytes)?;
    if &header_bytes[..8] != MAGIC
        || &header_bytes[8..24] != source.0.as_bytes()
        || u64::from_le_bytes(header_bytes[24..32].try_into().expect("fixed slice")) != generation
        || u32::from_le_bytes(header_bytes[32..36].try_into().expect("fixed slice")) as usize
            != page_records
        || u32::from_le_bytes(header_bytes[36..40].try_into().expect("fixed slice")) as usize
            != page_bytes
        || header_bytes[40..56] != journal_identity
        || u32::from_le_bytes(header_bytes[56..60].try_into().expect("fixed slice"))
            != hash(&header_bytes[..56])
    {
        return Err(invalid("index header mismatch"));
    }
    let count = (length - HEADER_LEN) / ENTRY_LEN;
    let mut previous_sequence = None;
    let mut next_offset = 0;
    let mut committed_count = 0;
    let mut group_start = 0;
    let mut group_page = None;
    let mut group_last = None;
    for position in 0..count {
        let mut bytes = [0u8; ENTRY_LEN as usize];
        file.read_exact(&mut bytes)?;
        let entry = decode_entry(&bytes)?;
        if entry.position != position
            || previous_sequence.is_some_and(|value| value >= entry.sequence)
            || entry.next_offset < entry.page_offset
        {
            return Err(invalid("invalid index ordering"));
        }
        if group_page != Some(entry.page_offset) {
            if let Some(last) = group_last {
                if !page_committed(last) {
                    file.set_len(HEADER_LEN + group_start * ENTRY_LEN)?;
                    return Ok((
                        committed_count,
                        next_offset,
                        sequence_at(file, committed_count)?,
                    ));
                }
                committed_count = position;
                next_offset = last.next_offset;
            }
            group_start = position;
            group_page = Some(entry.page_offset);
        } else if group_last.is_some_and(page_committed) {
            return Err(invalid("index page commit is not final entry"));
        }
        let expected_within = position - group_start;
        if u64::from(entry.within_page) != expected_within {
            return Err(invalid("invalid index page position"));
        }
        previous_sequence = Some(entry.sequence);
        group_last = Some(entry);
    }
    if let Some(last) = group_last {
        if page_committed(last) {
            committed_count = count;
            next_offset = last.next_offset;
        } else {
            file.set_len(HEADER_LEN + group_start * ENTRY_LEN)?;
            previous_sequence = sequence_at(file, committed_count)?;
        }
    }
    Ok((committed_count, next_offset, previous_sequence))
}

fn page_committed(entry: IndexEntry) -> bool {
    entry.next_offset > entry.page_offset
}

fn sequence_at(file: &mut File, count: u64) -> io::Result<Option<u64>> {
    if count == 0 {
        return Ok(None);
    }
    file.seek(SeekFrom::Start(HEADER_LEN + (count - 1) * ENTRY_LEN))?;
    let mut bytes = [0; ENTRY_LEN as usize];
    file.read_exact(&mut bytes)?;
    Ok(Some(decode_entry(&bytes)?.sequence))
}

fn encode_entry(entry: IndexEntry) -> [u8; ENTRY_LEN as usize] {
    let mut bytes = [0u8; ENTRY_LEN as usize];
    bytes[..8].copy_from_slice(&entry.sequence.to_le_bytes());
    bytes[8..16].copy_from_slice(&entry.position.to_le_bytes());
    bytes[16..24].copy_from_slice(&entry.page_offset.to_le_bytes());
    bytes[24..32].copy_from_slice(&entry.next_offset.to_le_bytes());
    bytes[32..36].copy_from_slice(&entry.within_page.to_le_bytes());
    let checksum = hash(&bytes[..36]);
    bytes[36..40].copy_from_slice(&checksum.to_le_bytes());
    bytes
}

fn decode_entry(bytes: &[u8; ENTRY_LEN as usize]) -> io::Result<IndexEntry> {
    let expected = u32::from_le_bytes(bytes[36..40].try_into().expect("fixed slice"));
    if hash(&bytes[..36]) != expected {
        return Err(invalid("index entry checksum mismatch"));
    }
    Ok(IndexEntry {
        sequence: u64::from_le_bytes(bytes[..8].try_into().expect("fixed slice")),
        position: u64::from_le_bytes(bytes[8..16].try_into().expect("fixed slice")),
        page_offset: u64::from_le_bytes(bytes[16..24].try_into().expect("fixed slice")),
        next_offset: u64::from_le_bytes(bytes[24..32].try_into().expect("fixed slice")),
        within_page: u32::from_le_bytes(bytes[32..36].try_into().expect("fixed slice")),
    })
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod budget_failure_tests {
    use super::*;
    use lvu_core::{ChunkPosition, RecordId, StreamKind};
    use tempfile::TempDir;
    use uuid::Uuid;

    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn record(source: SourceId, sequence: u64) -> RawRecord {
        RawRecord {
            record_id: RecordId {
                source_id: source,
                sequence,
            },
            captured_at_unix_nanos: 0,
            stream: StreamKind::File,
            bytes: Vec::new(),
            delimiter: Vec::new(),
            acquisition_id: Uuid::new_v4(),
            chunk: ChunkPosition::Complete,
        }
    }

    fn budget() -> IndexBudget {
        IndexBudget {
            per_source: 1024,
            total: 1024,
            reconciliation_limit: 64,
        }
    }

    #[test]
    fn append_write_and_flush_failures_never_undercount_partial_bytes() {
        let _serial = SERIAL.lock().unwrap();
        for point in [FaultPoint::EntryWrite, FaultPoint::CommitFlush] {
            let root = TempDir::new().unwrap();
            let path = root
                .path()
                .join("00000000-0000-0000-0000-000000000001.rows.idx");
            let source = SourceId::new();
            let (mut index, _, _) =
                DiskIndex::open_budgeted(&path, source, 1, 4, 1024, [0; 16], budget()).unwrap();
            *INJECTED_FAILURE.lock().unwrap() = Some((path.clone(), point));
            assert!(
                index
                    .append_page(0, 10, &[record(source, 0), record(source, 1)], 1024)
                    .is_err()
            );
            let actual = std::fs::metadata(&path).unwrap().len();
            let accounted = read_budget(root.path()).unwrap();
            assert!(accounted.verified);
            assert_eq!(accounted.bytes, actual);
            assert_eq!(actual, HEADER_LEN);
        }
    }

    #[test]
    fn failed_rebuild_reconciles_the_post_failure_size() {
        let _serial = SERIAL.lock().unwrap();
        let root = TempDir::new().unwrap();
        let path = root
            .path()
            .join("00000000-0000-0000-0000-000000000002.rows.idx");
        std::fs::write(&path, []).unwrap();
        *INJECTED_FAILURE.lock().unwrap() = Some((path.clone(), FaultPoint::RebuildFlush));
        assert!(
            DiskIndex::open_budgeted(&path, SourceId::new(), 1, 4, 1024, [0; 16], budget())
                .is_err()
        );
        let actual = std::fs::metadata(&path).unwrap().len();
        let accounted = read_budget(root.path()).unwrap();
        assert!(accounted.verified);
        assert_eq!(accounted.bytes, actual);
        assert_eq!(actual, HEADER_LEN);
    }

    #[test]
    fn legacy_header_remains_recognizable_only_for_owned_cleanup() {
        let root = TempDir::new().unwrap();
        let source = SourceId::new();
        let path = root.path().join(format!("{}.rows.idx", source.0));
        let mut bytes = [0u8; OLD_HEADER_LEN as usize];
        bytes[..8].copy_from_slice(OLD_MAGIC);
        bytes[8..24].copy_from_slice(source.0.as_bytes());
        bytes[24..32].copy_from_slice(&1u64.to_le_bytes());
        bytes[32..36].copy_from_slice(&4u32.to_le_bytes());
        bytes[36..40].copy_from_slice(&1024u32.to_le_bytes());
        let checksum = hash(&bytes[..40]);
        bytes[40..44].copy_from_slice(&checksum.to_le_bytes());
        std::fs::write(&path, bytes).unwrap();

        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        validate_owned_artifact(&mut file, source.0.as_bytes(), 1024, || false).unwrap();
        drop(file);
        let (_, rebuilt, _) =
            DiskIndex::open_budgeted(&path, source, 1, 4, 1024, [9; 16], budget()).unwrap();
        assert!(rebuilt, "legacy offsets must never be served as current");
    }
}
