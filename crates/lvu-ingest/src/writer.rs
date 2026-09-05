use crate::{
    catalog::{Catalog, CatalogEvent},
    cursor::{self, DurableFileCursor},
    manager::{RuntimeConfig, RuntimeError, RuntimeState, SourceProgress},
};
use lvu_core::{
    CaptureEvent, FileCaptureResume, FileContentHasher, FileEncoding, FileIdentity,
    FileResumeCursor, Journal, JournalPage, RawRecord, RecordId, SourceId,
    acquisition::BoundaryReason,
};
use std::{
    fs,
    io::Read,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, oneshot, watch};

pub(crate) enum WriterMessage {
    Event {
        event: CaptureEvent,
        _permit: OwnedSemaphorePermit,
    },
    Page {
        offset: u64,
        max_records: usize,
        max_bytes: usize,
        reply: oneshot::Sender<Result<JournalPage, RuntimeError>>,
        _permit: OwnedSemaphorePermit,
    },
    Finish {
        state: RuntimeState,
        discarded_bytes: u64,
        discarded_bytes_known: bool,
        reply: oneshot::Sender<Result<(), RuntimeError>>,
    },
}

pub(crate) struct WriterInit {
    pub sender: tokio::sync::mpsc::Sender<WriterMessage>,
    pub slots: Arc<Semaphore>,
    pub task: tokio::task::JoinHandle<()>,
    pub resume: Option<FileCaptureResume>,
}

#[derive(Clone)]
pub(crate) struct StartupCancellation {
    pub shutting_down: Arc<AtomicBool>,
    pub caller_status: watch::Receiver<()>,
}

pub(crate) struct FileCursorSetup {
    pub cursor_path: PathBuf,
    pub source_path: PathBuf,
    pub durable: Option<DurableFileCursor>,
    pub cancellation: StartupCancellation,
}

impl StartupCancellation {
    fn cancelled(&self) -> bool {
        self.shutting_down.load(Ordering::Acquire) || self.caller_status.has_changed().is_err()
    }
}

struct FileCursorWriter {
    cursor_path: PathBuf,
    source_path: PathBuf,
    durable: Option<DurableFileCursor>,
}

pub(crate) async fn spawn_writer(
    source_id: SourceId,
    journal_path: PathBuf,
    catalog_path: PathBuf,
    file_cursor: Option<FileCursorSetup>,
    config: RuntimeConfig,
    progress: watch::Sender<SourceProgress>,
    mut initial: SourceProgress,
) -> Result<WriterInit, RuntimeError> {
    let open_path = journal_path.clone();
    let (journal, recovery, mut catalog, file_cursor) = tokio::task::spawn_blocking(move || {
        let (mut journal, recovery) = Journal::open(&open_path, source_id)?;
        let catalog = Catalog::open(&catalog_path)?;
        let file_cursor = recover_file_cursor(&mut journal, file_cursor)?;
        Ok::<_, RuntimeError>((journal, recovery, catalog, file_cursor))
    })
    .await??;
    initial.records = recovery.records;
    initial.high_watermark = recovery.last_sequence.map(|sequence| RecordId {
        source_id,
        sequence,
    });
    initial.journal_bytes = fs::metadata(&journal_path)?.len();
    initial.synced_records = recovery.records;
    initial.state = RuntimeState::Running;
    catalog.record(CatalogEvent::Running)?;
    let _ = progress.send(initial.clone());

    let capacity = config.writer_queue_capacity.saturating_add(1);
    let (sender, receiver) = tokio::sync::mpsc::channel(capacity);
    let slots = Arc::new(Semaphore::new(config.writer_queue_capacity));
    let resume = file_cursor
        .as_ref()
        .and_then(|state| state.durable.as_ref())
        .map(|state| FileCaptureResume {
            cursor: state.file.clone(),
            encoding: state.encoding.clone(),
        });
    let task = tokio::task::spawn_blocking(move || {
        if let Err(error) = run_writer(
            journal,
            catalog,
            config,
            progress.clone(),
            initial,
            receiver,
            file_cursor,
        ) {
            let mut failed = progress.borrow().clone();
            if !matches!(failed.state, RuntimeState::StorageBlocked) {
                failed.state = RuntimeState::Error;
            }
            failed.last_error = Some(error.to_string());
            let _ = progress.send(failed);
        }
    });
    Ok(WriterInit {
        sender,
        slots,
        task,
        resume,
    })
}

fn run_writer(
    mut journal: Journal,
    mut catalog: Catalog,
    config: RuntimeConfig,
    progress: watch::Sender<SourceProgress>,
    mut current: SourceProgress,
    mut receiver: tokio::sync::mpsc::Receiver<WriterMessage>,
    mut file_cursor: Option<FileCursorWriter>,
) -> Result<(), RuntimeError> {
    let mut batch = Vec::with_capacity(config.batch_records);
    let mut batches_since_sync = 0usize;
    while let Some(message) = receiver.blocking_recv() {
        match message {
            WriterMessage::Event {
                event: CaptureEvent::Record(record),
                ..
            } => {
                batch.push(record.into_raw(current.source_id));
                while batch.len() < config.batch_records {
                    match receiver.try_recv() {
                        Ok(WriterMessage::Event {
                            event: CaptureEvent::Record(record),
                            ..
                        }) => {
                            batch.push(record.into_raw(current.source_id));
                        }
                        Ok(other) => {
                            append_batch(
                                &mut journal,
                                &mut catalog,
                                &mut batch,
                                &config,
                                &mut current,
                                &progress,
                                &mut batches_since_sync,
                            )?;
                            if handle_non_record(
                                other,
                                &mut journal,
                                &mut catalog,
                                &config,
                                &mut current,
                                &progress,
                                &mut file_cursor,
                            )? {
                                return Ok(());
                            }
                            break;
                        }
                        Err(_) => break,
                    }
                }
                append_batch(
                    &mut journal,
                    &mut catalog,
                    &mut batch,
                    &config,
                    &mut current,
                    &progress,
                    &mut batches_since_sync,
                )?;
            }
            other => {
                if handle_non_record(
                    other,
                    &mut journal,
                    &mut catalog,
                    &config,
                    &mut current,
                    &progress,
                    &mut file_cursor,
                )? {
                    return Ok(());
                }
            }
        }
    }
    journal.sync_data()?;
    current.synced_records = current.records;
    current.state = RuntimeState::Incomplete;
    current.last_error = Some("writer channel closed without completion".into());
    catalog.record(CatalogEvent::Incomplete {
        discarded_buffered_bytes: current.discarded_bytes,
        discarded_bytes_known: false,
        reason: "writer channel closed",
    })?;
    let _ = progress.send(current);
    Ok(())
}

fn append_batch(
    journal: &mut Journal,
    catalog: &mut Catalog,
    batch: &mut Vec<RawRecord>,
    config: &RuntimeConfig,
    current: &mut SourceProgress,
    progress: &watch::Sender<SourceProgress>,
    batches_since_sync: &mut usize,
) -> Result<(), RuntimeError> {
    if batch.is_empty() {
        return Ok(());
    }
    if !config.writer_delay.is_zero() {
        std::thread::sleep(config.writer_delay);
    }
    let mut records = std::mem::take(batch).into_iter();
    while let Some(record) = records.next() {
        let estimated = record.bytes.len() as u64 + record.delimiter.len() as u64 + 80;
        if let Some(limit) = config.storage_limit_bytes
            && current.journal_bytes.saturating_add(estimated) > limit
        {
            let rejected_bytes = record.bytes.len() as u64
                + record.delimiter.len() as u64
                + records
                    .as_slice()
                    .iter()
                    .map(|pending| (pending.bytes.len() + pending.delimiter.len()) as u64)
                    .sum::<u64>();
            current.state = RuntimeState::StorageBlocked;
            current.discarded_bytes = current.discarded_bytes.saturating_add(rejected_bytes);
            current.discarded_bytes_known = false;
            current.last_error = Some(format!("durable capture limit {limit} bytes reached"));
            catalog.record(CatalogEvent::StorageBlocked {
                limit_bytes: limit,
                discarded_buffered_bytes: current.discarded_bytes,
                discarded_bytes_known: false,
            })?;
            let _ = progress.send(current.clone());
            return Err(RuntimeError::StorageLimit { limit });
        }
        let id = journal.append(record)?;
        current.records += 1;
        current.high_watermark = Some(id);
        current.journal_bytes = fs::metadata(journal.path())?.len();
    }
    journal.flush()?;
    *batches_since_sync += 1;
    if *batches_since_sync >= config.sync_every_batches {
        journal.sync_data()?;
        *batches_since_sync = 0;
        current.synced_records = current.records;
    }
    let externally_visible = progress.borrow().state;
    if matches!(
        externally_visible,
        RuntimeState::Stopping | RuntimeState::Aborting
    ) {
        current.state = externally_visible;
    }
    let _ = progress.send(current.clone());
    Ok(())
}

fn handle_non_record(
    message: WriterMessage,
    journal: &mut Journal,
    catalog: &mut Catalog,
    config: &RuntimeConfig,
    current: &mut SourceProgress,
    progress: &watch::Sender<SourceProgress>,
    file_cursor: &mut Option<FileCursorWriter>,
) -> Result<bool, RuntimeError> {
    match message {
        WriterMessage::Event { event, .. } => {
            match event {
                CaptureEvent::Boundary {
                    acquisition_id,
                    reason,
                } => {
                    current.boundaries += 1;
                    catalog.record(CatalogEvent::Boundary {
                        acquisition_id,
                        reason: boundary_name(reason),
                    })?;
                }
                CaptureEvent::CommandExit {
                    acquisition_id,
                    status,
                } => {
                    current.exit_code = status.code();
                    catalog.record(CatalogEvent::CommandExit {
                        acquisition_id,
                        code: status.code(),
                        success: status.success(),
                    })?;
                }
                CaptureEvent::Error {
                    acquisition_id,
                    message,
                } => {
                    current.last_error = Some(message.clone());
                    catalog.record(CatalogEvent::Error {
                        acquisition_id,
                        message: &message,
                    })?;
                }
                CaptureEvent::FileCheckpoint {
                    acquisition_id,
                    cursor: checkpoint,
                    encoding,
                } => {
                    let state = file_cursor.as_mut().ok_or_else(|| {
                        RuntimeError::Io(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "file checkpoint received for non-file source",
                        ))
                    })?;
                    journal.sync_data()?;
                    current.synced_records = current.records;
                    let durable = DurableFileCursor {
                        schema_version: 1,
                        source_id: current.source_id,
                        path: state.source_path.clone(),
                        acquisition_id,
                        journal_offset: journal.end_offset()?,
                        file: checkpoint,
                        encoding,
                    };
                    cursor::store(&state.cursor_path, &durable)?;
                    state.durable = Some(durable);
                }
                CaptureEvent::Stopped { .. } | CaptureEvent::Record(_) => {}
            }
            let _ = progress.send(current.clone());
            Ok(false)
        }
        WriterMessage::Page {
            offset,
            max_records,
            max_bytes,
            reply,
            ..
        } => {
            let result = journal
                .read_page(
                    offset,
                    max_records.min(config.max_page_records),
                    max_bytes.min(config.max_page_bytes),
                )
                .map_err(RuntimeError::from);
            let _ = reply.send(result);
            Ok(false)
        }
        WriterMessage::Finish {
            state,
            discarded_bytes,
            discarded_bytes_known,
            reply,
        } => {
            let state = if reply.is_closed() {
                RuntimeState::Incomplete
            } else {
                state
            };
            let result = finish_writer(
                journal,
                catalog,
                state,
                discarded_bytes,
                discarded_bytes_known,
                current,
            );
            let successful = result.is_ok();
            if successful {
                // Publish durable counters, but leave terminal-state ownership
                // to the supervisor after it joins this writer and releases the
                // runtime lease. Publishing `current.state` here permits reopen
                // before cleanup has finished.
                let mut durable = current.clone();
                durable.state = progress.borrow().state;
                let _ = progress.send(durable);
            }
            let _ = reply.send(result);
            Ok(successful)
        }
    }
}

fn finish_writer(
    journal: &mut Journal,
    catalog: &mut Catalog,
    state: RuntimeState,
    discarded_bytes: u64,
    discarded_bytes_known: bool,
    current: &mut SourceProgress,
) -> Result<(), RuntimeError> {
    journal.sync_data()?;
    current.journal_bytes = fs::metadata(journal.path())?.len();
    current.synced_records = current.records;
    current.state = state;
    current.discarded_bytes = discarded_bytes;
    current.discarded_bytes_known = discarded_bytes_known;
    match state {
        RuntimeState::Stopped => catalog.record(CatalogEvent::Stopped)?,
        RuntimeState::Aborted => catalog.record(CatalogEvent::Aborted {
            discarded_buffered_bytes: discarded_bytes,
            discarded_bytes_known,
        })?,
        RuntimeState::Incomplete => catalog.record(CatalogEvent::Incomplete {
            discarded_buffered_bytes: discarded_bytes,
            discarded_bytes_known,
            reason: "shutdown deadline exceeded",
        })?,
        _ => {}
    }
    Ok(())
}

fn boundary_name(reason: BoundaryReason) -> &'static str {
    match reason {
        BoundaryReason::Started => "started",
        BoundaryReason::Rotated => "rotated",
        BoundaryReason::Truncated => "truncated",
    }
}

fn recover_file_cursor(
    journal: &mut Journal,
    setup: Option<FileCursorSetup>,
) -> Result<Option<FileCursorWriter>, RuntimeError> {
    const MAX_RECOVERY_RECORDS: usize = 65_536;
    const MAX_RECOVERY_BYTES: u64 = 8 * 1024 * 1024;
    let Some(FileCursorSetup {
        cursor_path,
        source_path,
        durable,
        cancellation,
    }) = setup
    else {
        return Ok(None);
    };
    let Some(mut durable) = durable else {
        return Ok(Some(FileCursorWriter {
            cursor_path,
            source_path,
            durable: None,
        }));
    };
    let journal_end = journal.end_offset()?;
    if durable.journal_offset > journal_end {
        return Err(RuntimeError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "file cursor points beyond the journal",
        )));
    }
    if durable.journal_offset == journal_end {
        return Ok(Some(FileCursorWriter {
            cursor_path,
            source_path,
            durable: Some(durable),
        }));
    }
    if cancellation.cancelled() {
        return Err(RuntimeError::Closed);
    }
    let source_file = fs::File::open(&source_path)?;
    let metadata = source_file.metadata()?;
    if file_identity(&metadata) != durable.file.identity {
        return Ok(Some(FileCursorWriter {
            cursor_path,
            source_path,
            durable: Some(durable),
        }));
    }
    if matches!(durable.encoding, FileEncoding::Plain) && metadata.len() < durable.file.offset {
        return Ok(Some(FileCursorWriter {
            cursor_path,
            source_path,
            durable: Some(durable),
        }));
    }
    if !validate_source_encoding(&source_path, &durable.encoding, &cancellation)? {
        return Ok(Some(FileCursorWriter {
            cursor_path,
            source_path,
            durable: Some(durable),
        }));
    }
    let mut source: Box<dyn Read> = match durable.encoding {
        FileEncoding::Plain => Box::new(source_file),
        FileEncoding::Gzip { .. } => Box::new(flate2::read::MultiGzDecoder::new(source_file)),
    };
    let Some(mut hasher) =
        validate_acknowledged_prefix(source.as_mut(), &durable.file, &cancellation)?
    else {
        return Ok(Some(FileCursorWriter {
            cursor_path,
            source_path,
            durable: Some(durable),
        }));
    };
    let mut journal_offset = durable.journal_offset;
    let mut recovered_bytes = 0_u64;
    let mut recovered_records = 0_usize;
    while journal_offset < journal_end {
        if cancellation.cancelled() {
            return Err(RuntimeError::Closed);
        }
        if recovered_records == MAX_RECOVERY_RECORDS || recovered_bytes >= MAX_RECOVERY_BYTES {
            return Err(RuntimeError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "uncheckpointed file journal tail exceeds recovery bounds",
            )));
        }
        let page = journal.read_page(journal_offset, 1, 2 * 1024 * 1024)?;
        let record = page.records.first().ok_or_else(|| {
            RuntimeError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "file cursor journal tail made no progress",
            ))
        })?;
        if record.stream != lvu_core::StreamKind::File
            || record.acquisition_id != durable.acquisition_id
        {
            return Err(RuntimeError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "file cursor journal tail crosses an uncheckpointed boundary",
            )));
        }
        let expected: Vec<_> = record
            .bytes
            .iter()
            .chain(&record.delimiter)
            .copied()
            .collect();
        let mut actual = vec![0; expected.len()];
        source.read_exact(&mut actual)?;
        if actual != expected {
            return Err(RuntimeError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "file content no longer matches the uncheckpointed journal tail",
            )));
        }
        durable.file.offset += expected.len() as u64;
        hasher.update(&expected);
        durable.file.evidence.extend_from_slice(&expected);
        if durable.file.evidence.len() > 4096 {
            durable
                .file
                .evidence
                .drain(..durable.file.evidence.len() - 4096);
        }
        recovered_bytes += expected.len() as u64;
        recovered_records += 1;
        journal_offset = page.next_offset;
    }
    if journal_offset != durable.journal_offset {
        durable.journal_offset = journal_offset;
        durable.file.content_crc32 = hasher.finalize();
        cursor::store(&cursor_path, &durable)?;
    }
    Ok(Some(FileCursorWriter {
        cursor_path,
        source_path,
        durable: Some(durable),
    }))
}

fn validate_acknowledged_prefix(
    source: &mut dyn Read,
    cursor: &FileResumeCursor,
    cancellation: &StartupCancellation,
) -> Result<Option<FileContentHasher>, RuntimeError> {
    if cursor.evidence.len() as u64 > cursor.offset {
        return Ok(None);
    }
    let mut remaining = cursor.offset;
    let mut hasher = FileContentHasher::new();
    let mut tail = Vec::new();
    let mut buffer = [0_u8; 64 * 1024];
    while remaining > 0 {
        if cancellation.cancelled() {
            return Err(RuntimeError::Closed);
        }
        let limit = remaining.min(buffer.len() as u64) as usize;
        let count = source.read(&mut buffer[..limit])?;
        if count == 0 {
            return Ok(None);
        }
        hasher.update(&buffer[..count]);
        tail.extend_from_slice(&buffer[..count]);
        if tail.len() > 4096 {
            tail.drain(..tail.len() - 4096);
        }
        remaining -= count as u64;
    }
    if hasher.checksum() != cursor.content_crc32 || tail != cursor.evidence {
        return Ok(None);
    }
    Ok(Some(hasher))
}

fn validate_source_encoding(
    path: &PathBuf,
    encoding: &FileEncoding,
    cancellation: &StartupCancellation,
) -> Result<bool, RuntimeError> {
    let FileEncoding::Gzip {
        compressed_size,
        compressed_crc32,
        compressed_evidence,
    } = encoding
    else {
        return Ok(true);
    };
    let mut source = fs::File::open(path)?;
    if source.metadata()?.len() != *compressed_size {
        return Ok(false);
    }
    let mut hasher = FileContentHasher::new();
    let mut tail = Vec::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        if cancellation.cancelled() {
            return Err(RuntimeError::Closed);
        }
        let count = source.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
        tail.extend_from_slice(&buffer[..count]);
        if tail.len() > 4096 {
            tail.drain(..tail.len() - 4096);
        }
    }
    Ok(hasher.finalize() == *compressed_crc32 && tail == *compressed_evidence)
}

#[cfg(unix)]
fn file_identity(metadata: &fs::Metadata) -> Option<FileIdentity> {
    use std::os::unix::fs::MetadataExt;
    Some(FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(not(unix))]
fn file_identity(_: &fs::Metadata) -> Option<FileIdentity> {
    None
}
