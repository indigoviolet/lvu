use crc32fast::hash;
use fs2::FileExt;
use lvu_core::{RawRecord, SourceId};
use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::Path,
};

const MAGIC: &[u8; 8] = b"LVUIDX2\0";
const HEADER_LEN: u64 = 44;
const ENTRY_LEN: u64 = 40;

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
    pub count: u64,
    pub next_offset: u64,
    pub high_sequence: Option<u64>,
}

impl DiskIndex {
    pub fn open(
        path: &Path,
        source: SourceId,
        generation: u64,
        page_records: usize,
        page_bytes: usize,
        maximum_bytes: u64,
    ) -> io::Result<(Self, bool)> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
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
            maximum_bytes,
        ) {
            Ok(metadata) => {
                return Ok((
                    Self {
                        file,
                        count: metadata.0,
                        next_offset: metadata.1,
                        high_sequence: metadata.2,
                    },
                    false,
                ));
            }
            Err(_) => true,
        };
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&header(source, generation, page_records, page_bytes)?)?;
        file.flush()?;
        Ok((
            Self {
                file,
                count: 0,
                next_offset: 0,
                high_sequence: None,
            },
            rebuilt,
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
        }
        self.file.flush()?;
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
        self.file.seek(SeekFrom::Start(
            original_length + (records.len() as u64 - 1) * ENTRY_LEN,
        ))?;
        self.file.write_all(&encode_entry(committed))?;
        self.file.flush()?;
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

fn header(
    source: SourceId,
    generation: u64,
    page_records: usize,
    page_bytes: usize,
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
    let checksum = hash(&bytes[..40]);
    bytes[40..44].copy_from_slice(&checksum.to_le_bytes());
    Ok(bytes)
}

fn validate(
    file: &mut File,
    source: SourceId,
    generation: u64,
    page_records: usize,
    page_bytes: usize,
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
        || u32::from_le_bytes(header_bytes[40..44].try_into().expect("fixed slice"))
            != hash(&header_bytes[..40])
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
