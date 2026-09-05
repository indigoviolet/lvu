use crate::{ChunkPosition, RawRecord, RecordId, SourceId, StreamKind};
use crc32fast::hash;
use fs2::FileExt;
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
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

pub struct Journal {
    source_id: SourceId,
    path: PathBuf,
    seq_path: PathBuf,
    file: File,
    _lock: File,
    next_sequence: u64,
    reserved_until: u64,
    poisoned: bool,
}

impl Journal {
    pub fn open(
        path: impl AsRef<Path>,
        source_id: SourceId,
    ) -> Result<(Self, Recovery), JournalError> {
        let path = path.as_ref().to_owned();
        let seq_path = sibling_path(&path, ".seq");
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(sibling_path(&path, ".lock"))?;
        if let Err(error) = FileExt::try_lock_exclusive(&lock) {
            return if error.kind() == io::ErrorKind::WouldBlock {
                Err(JournalError::AlreadyOpen)
            } else {
                Err(JournalError::Io(error))
            };
        }
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
    let mut cursor = offset;
    let mut decoded_bytes = 0usize;
    let mut records = Vec::new();
    while cursor < total && records.len() < max_records {
        let (record, frame_len) = read_complete_frame(file, cursor, total, source_id)?;
        let record_bytes = record.bytes.len() + record.delimiter.len();
        if !records.is_empty() && decoded_bytes.saturating_add(record_bytes) > max_bytes {
            file.seek(SeekFrom::Start(cursor))?;
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

fn read_header(file: &mut File, offset: u64) -> Result<FrameHeader, JournalError> {
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
    file: &mut File,
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
    file: &mut File,
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
}
