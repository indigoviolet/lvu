use lvu_core::{ChunkPosition, Journal, JournalError, RawRecord, RecordId, SourceId, StreamKind};
use std::{
    fs::{self, OpenOptions},
    io::{Seek, SeekFrom, Write},
};
use tempfile::tempdir;
use uuid::Uuid;

fn record(source_id: SourceId, bytes: &[u8], delimiter: &[u8]) -> RawRecord {
    RawRecord {
        record_id: RecordId {
            source_id,
            sequence: 0,
        },
        captured_at_unix_nanos: 42,
        stream: StreamKind::File,
        bytes: bytes.to_vec().into(),
        delimiter: delimiter.to_vec().into(),
        acquisition_id: Uuid::new_v4(),
        chunk: ChunkPosition::Complete,
    }
}

#[test]
fn checksummed_header_corruption_is_not_truncated() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("header.lvu");
    let source = SourceId::new();
    let (mut journal, _) = Journal::open(&path, source).unwrap();
    journal.append(record(source, b"precious", b"\n")).unwrap();
    journal.sync_data().unwrap();
    drop(journal);
    let mut bytes = fs::read(&path).unwrap();
    bytes[4..8].copy_from_slice(&1000_u32.to_le_bytes());
    fs::write(&path, &bytes).unwrap();
    assert!(matches!(
        Journal::open(&path, source),
        Err(JournalError::Corrupt {
            reason: "header checksum mismatch",
            ..
        })
    ));
    assert_eq!(fs::read(path).unwrap(), bytes);
}

/// Removes a relative journal and its sidecars however the test ends.
///
/// A `TempDir` would be the ordinary answer, but it would retire the case this
/// test exists for: a bare file name has *no* parent directory, and that is the
/// path `Journal::open` handles separately when it normalises the lock path.
/// Put the files under a temporary directory and the argument is no longer
/// relative in the way that matters.
///
/// So they are still written where the test runs, and cleaning up is no longer
/// the last statement of a body that a panic can skip. It was: twelve zero-byte
/// strays from a killed run reached a commit that way.
struct RelativeJournals {
    stems: Vec<String>,
}

impl Drop for RelativeJournals {
    fn drop(&mut self) {
        for stem in &self.stems {
            for suffix in ["", ".seq", ".seq.tmp", ".lock"] {
                let _ = fs::remove_file(format!("{stem}{suffix}"));
            }
        }
    }
}

#[test]
fn relative_paths_writer_lock_and_collision_safe_sidecars_work() {
    let stem = format!("lvu-journal-test-{}", Uuid::new_v4());
    let first_path = format!("{stem}.one");
    let second_path = format!("{stem}.two");
    // Declared before the journals, so it is dropped after them: the files are
    // closed by the time anything tries to remove them.
    let _cleanup = RelativeJournals {
        stems: vec![first_path.clone(), second_path.clone()],
    };
    let source = SourceId::new();
    let (mut first, _) = Journal::open(&first_path, source).unwrap();
    assert!(matches!(
        Journal::open(&first_path, source),
        Err(JournalError::AlreadyOpen)
    ));
    let (mut second, _) = Journal::open(&second_path, source).unwrap();
    first.append(record(source, b"one", b"\n")).unwrap();
    second.append(record(source, b"two", b"\n")).unwrap();
}

#[test]
fn bounded_pages_cover_each_record_once() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("pages.lvu");
    let source = SourceId::new();
    let (mut journal, _) = Journal::open(&path, source).unwrap();
    for value in [b"aa".as_slice(), b"bbb", b"cccc"] {
        journal.append(record(source, value, b"\n")).unwrap();
    }
    let first = journal.read_page(0, 2, 1024).unwrap();
    assert_eq!(
        first
            .records
            .iter()
            .map(|value| value.bytes.as_slice())
            .collect::<Vec<_>>(),
        [b"aa".as_slice(), b"bbb"]
    );
    assert!(!first.end_of_journal);
    let second = journal.read_page(first.next_offset, 2, 1024).unwrap();
    assert_eq!(second.records.len(), 1);
    assert_eq!(second.records[0].bytes, b"cccc");
    assert!(second.end_of_journal);
}

#[test]
fn failed_page_read_cannot_reposition_the_append_writer() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("page-error.lvu");
    let source = SourceId::new();
    let (mut journal, _) = Journal::open(&path, source).unwrap();
    journal.append(record(source, b"first", b"\n")).unwrap();
    journal.sync_data().unwrap();
    let original = fs::read(&path).unwrap();

    assert!(journal.read_page(1, 1, 1024).is_err());
    journal.append(record(source, b"second", b"\n")).unwrap();
    journal.sync_data().unwrap();

    assert!(fs::read(path).unwrap().starts_with(&original));
}

#[test]
fn corrupted_page_read_cannot_reposition_the_append_writer() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("corrupt-page.lvu");
    let source = SourceId::new();
    let (mut journal, _) = Journal::open(&path, source).unwrap();
    journal.append(record(source, b"first", b"\n")).unwrap();
    journal.sync_data().unwrap();
    let mut corrupted = fs::read(&path).unwrap();
    *corrupted.last_mut().unwrap() ^= 0xff;
    fs::write(&path, &corrupted).unwrap();

    assert!(journal.read_page(0, 1, 1024).is_err());
    journal.append(record(source, b"second", b"\n")).unwrap();
    journal.sync_data().unwrap();

    assert!(fs::read(path).unwrap().starts_with(&corrupted));
}

#[test]
fn exact_bytes_invalid_utf8_and_delimiters_survive_restart() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("capture.lvu");
    let source = SourceId::new();
    let (mut journal, recovery) = Journal::open(&path, source).unwrap();
    assert_eq!(recovery.next_sequence, 0);
    assert_eq!(
        journal
            .append(record(source, b"bad\xff", b"\r\n"))
            .unwrap()
            .sequence,
        0
    );
    assert_eq!(
        journal
            .append(record(source, b"tail", b""))
            .unwrap()
            .sequence,
        1
    );
    journal.sync_data().unwrap();
    drop(journal);
    let (mut reopened, recovery) = Journal::open(&path, source).unwrap();
    assert_eq!(recovery.records, 2);
    assert_eq!(recovery.next_sequence, 1024);
    let records = reopened.collect_all_records().unwrap();
    assert_eq!(records[0].bytes, b"bad\xff");
    assert_eq!(records[0].delimiter, b"\r\n");
    assert_eq!(records[1].delimiter, b"");
    assert_eq!(
        reopened
            .append(record(source, b"next", b"\n"))
            .unwrap()
            .sequence,
        1024
    );
}

#[test]
fn torn_tail_is_truncated_but_reserved_sequence_is_not_reused() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("capture.lvu");
    let source = SourceId::new();
    let (mut j, _) = Journal::open(&path, source).unwrap();
    j.append(record(source, b"one", b"\n")).unwrap();
    j.sync_data().unwrap();
    let committed_len = fs::metadata(&path).unwrap().len();
    j.append(record(source, b"reserved-but-torn", b"\n"))
        .unwrap();
    j.sync_data().unwrap();
    drop(j);
    OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(committed_len + 10)
        .unwrap();
    let (mut j, r) = Journal::open(&path, source).unwrap();
    assert_eq!(r.truncated_tail_bytes, 10);
    assert_eq!(j.collect_all_records().unwrap().len(), 1);
    assert_eq!(
        j.append(record(source, b"two", b"\n")).unwrap().sequence,
        1024
    );
}

#[test]
fn fully_present_checksum_failure_is_committed_corruption() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("capture.lvu");
    let source = SourceId::new();
    let (mut j, _) = Journal::open(&path, source).unwrap();
    j.append(record(source, b"data", b"\n")).unwrap();
    j.sync_data().unwrap();
    drop(j);
    let mut f = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    f.seek(SeekFrom::End(-1)).unwrap();
    f.write_all(&[0xff]).unwrap();
    drop(f);
    assert!(matches!(
        Journal::open(&path, source),
        Err(JournalError::Corrupt {
            reason: "body checksum mismatch",
            ..
        })
    ));
}

#[test]
fn buffered_pages_preserve_boundaries_large_records_and_later_appends() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("buffered.lvu");
    let source = SourceId::new();
    let (mut journal, _) = Journal::open(&path, source).unwrap();
    let mut expected = Vec::new();
    for index in 0..900 {
        let bytes = if index == 400 {
            vec![0xff; 70 * 1024]
        } else {
            format!("record {index:04} {}", "x".repeat(80)).into_bytes()
        };
        journal.append(record(source, &bytes, b"\r\n")).unwrap();
        expected.push(bytes);
    }
    journal.sync_data().unwrap();
    let mut reader = lvu_core::JournalReader::open(&path, source).unwrap();
    let mut offset = 0;
    let mut actual = Vec::new();
    loop {
        let page = reader.read_page(offset, 37, 1500).unwrap();
        assert!(!page.records.is_empty());
        assert!(page.next_offset > offset);
        for row in page.records {
            assert_eq!(row.delimiter, b"\r\n");
            actual.push(row.bytes);
        }
        offset = page.next_offset;
        if page.end_of_journal {
            break;
        }
    }
    assert_eq!(actual, expected);
    journal
        .append(record(source, b"after cached EOF", b"\n"))
        .unwrap();
    journal.sync_data().unwrap();
    let appended = reader.read_page(offset, 1, 1).unwrap();
    assert_eq!(appended.records[0].bytes, b"after cached EOF");
    assert!(appended.end_of_journal);
    assert_eq!(journal.collect_all_records().unwrap().len(), 901);
}
