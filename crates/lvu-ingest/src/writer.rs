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
    // Captured before the blocking thread starts: `Handle::current` is only
    // available on a runtime thread, and the writer's is not one.
    let runtime = tokio::runtime::Handle::current();
    let task = tokio::task::spawn_blocking(move || {
        lvu_core::journal::trace::record(|| {
            format!(
                "run_writer ENTER source={source_id:?} thread={:?}",
                std::thread::current().id()
            )
        });
        let state = WriterState {
            commit: Commit::new(&config),
            journal,
            catalog,
            current: initial,
            progress: progress.clone(),
            file_cursor,
        };
        let outcome = run_writer(state, config, receiver, runtime);
        lvu_core::journal::trace::record(|| {
            format!(
                "run_writer RETURN source={source_id:?} ok={}",
                outcome.is_ok()
            )
        });
        if let Err(error) = outcome {
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

/// Group commit, and the only place capture asks for durability.
///
/// Durability is the expensive thing capture does, so the decision to pay for
/// it lives in one place with one rule: commit once `records` have accumulated
/// or once `interval` has passed since the last commit, whichever comes first.
///
/// The file cursor rides on the same decision. A cursor that names a file
/// offset whose records are not yet durable would, after a crash, resume past
/// data the journal does not hold — a loss, not a duplicate. So a checkpoint is
/// held here until the commit that covers it succeeds, and only then written.
/// Re-reading from a cursor that lags is the at-least-once behaviour the
/// framing contract already allows.
struct Commit {
    every_records: u64,
    interval: std::time::Duration,
    since_sync: u64,
    last: std::time::Instant,
    pending_cursor: Option<DurableFileCursor>,
    pending_cursor_path: Option<PathBuf>,
}

impl Commit {
    fn new(config: &RuntimeConfig) -> Self {
        Self {
            every_records: config.sync_every_records,
            interval: config.sync_interval,
            since_sync: 0,
            last: std::time::Instant::now(),
            pending_cursor: None,
            pending_cursor_path: None,
        }
    }

    fn appended(&mut self, records: u64) {
        self.since_sync = self.since_sync.saturating_add(records);
    }

    /// Work the next commit would make durable: uncommitted records, a held
    /// cursor, or both.
    fn outstanding(&self) -> bool {
        self.since_sync > 0 || self.pending_cursor.is_some()
    }

    fn due(&self) -> bool {
        self.outstanding()
            && (self.since_sync >= self.every_records || self.last.elapsed() >= self.interval)
    }

    /// How long the writer may wait for its next message before the time bound
    /// obliges it to commit. `None` when nothing is outstanding, so an idle
    /// source blocks rather than waking on a timer forever.
    fn deadline(&self) -> Option<std::time::Duration> {
        self.outstanding()
            .then(|| self.interval.saturating_sub(self.last.elapsed()))
    }

    /// Holds a checkpoint until it is covered. `journal_offset` was taken when
    /// the checkpoint arrived, so any later commit covers it; a newer
    /// checkpoint supersedes an older one that has not been written yet.
    fn defer_cursor(&mut self, path: PathBuf, cursor: DurableFileCursor) {
        self.pending_cursor_path = Some(path);
        self.pending_cursor = Some(cursor);
    }
}

/// Everything one source's writer owns. These travelled together through every
/// step as separate arguments; naming the group lets the commit policy reach
/// the journal and the cursor it decides for without each caller passing them.
struct WriterState {
    journal: Journal,
    catalog: Catalog,
    current: SourceProgress,
    progress: watch::Sender<SourceProgress>,
    file_cursor: Option<FileCursorWriter>,
    commit: Commit,
}

impl WriterState {
    /// Makes everything appended so far durable and, behind it, writes any
    /// cursor that was waiting to be covered.
    fn commit(&mut self) -> Result<(), RuntimeError> {
        // With nothing appended since the last commit there is nothing to make
        // durable, and a held cursor is already covered by that commit. Asking
        // the filesystem again would buy nothing.
        if self.commit.since_sync > 0 {
            self.journal.sync_data()?;
            self.commit.since_sync = 0;
            self.current.syncs += 1;
            self.current.synced_records = self.current.records;
        } else {
            self.journal.flush()?;
        }
        self.commit.last = std::time::Instant::now();
        // Only now may the cursor move: every record it accounts for is on the
        // disk.
        if let (Some(path), Some(cursor)) = (
            self.commit.pending_cursor_path.take(),
            self.commit.pending_cursor.take(),
        ) {
            cursor::store(&path, &cursor)?;
            if let Some(state) = self.file_cursor.as_mut() {
                state.durable = Some(cursor);
            }
        }
        Ok(())
    }

    fn commit_if_due(&mut self) -> Result<(), RuntimeError> {
        if self.commit.due() {
            self.commit()?;
        }
        Ok(())
    }

    fn publish(&self) {
        let _ = self.progress.send(self.current.clone());
    }
}

/// Waits for the next writer message, bounded by the commit deadline.
///
/// The writer is a blocking thread, so it cannot await; `block_on` here is safe
/// because this is not a runtime worker. `None` means the wait ended without a
/// message: either the deadline passed, or the channel closed. The caller
/// distinguishes the two by asking whether anything is still outstanding.
fn receive(
    runtime: &tokio::runtime::Handle,
    receiver: &mut tokio::sync::mpsc::Receiver<WriterMessage>,
    deadline: Option<std::time::Duration>,
) -> Option<WriterMessage> {
    match deadline {
        None => receiver.blocking_recv(),
        Some(remaining) => runtime
            .block_on(async { tokio::time::timeout(remaining, receiver.recv()).await })
            .ok()
            .flatten(),
    }
}

fn run_writer(
    mut state: WriterState,
    config: RuntimeConfig,
    mut receiver: tokio::sync::mpsc::Receiver<WriterMessage>,
    runtime: tokio::runtime::Handle,
) -> Result<(), RuntimeError> {
    let mut batch = Vec::with_capacity(config.batch_records);
    // The time half of group commit has to hold when nothing is arriving: a
    // tail that goes quiet must still become durable, and the cursor it is
    // holding must still be written. So the wait for the next message is
    // bounded by the commit deadline whenever there is anything outstanding,
    // and unbounded when there is not.
    loop {
        let Some(message) = receive(&runtime, &mut receiver, state.commit.deadline()) else {
            if state.commit.outstanding() {
                state.commit()?;
                state.publish();
                continue;
            }
            break;
        };
        match message {
            WriterMessage::Event {
                event: CaptureEvent::Records(records),
                ..
            } => {
                let source_id = state.current.source_id;
                state.current.handovers += 1;
                batch.extend(records.into_iter().map(|record| record.into_raw(source_id)));
                while batch.len() < config.batch_records {
                    match receiver.try_recv() {
                        Ok(WriterMessage::Event {
                            event: CaptureEvent::Records(records),
                            ..
                        }) => {
                            state.current.handovers += 1;
                            batch.extend(
                                records.into_iter().map(|record| record.into_raw(source_id)),
                            );
                        }
                        Ok(other) => {
                            append_batch(&mut state, &mut batch, &config)?;
                            if handle_non_record(&mut state, other, &config)? {
                                return Ok(());
                            }
                            break;
                        }
                        Err(_) => break,
                    }
                }
                append_batch(&mut state, &mut batch, &config)?;
            }
            other => {
                if handle_non_record(&mut state, other, &config)? {
                    return Ok(());
                }
            }
        }
    }
    // The channel closed without a Finish: commit what is held, then say so.
    state.commit()?;
    state.current.state = RuntimeState::Incomplete;
    state.current.last_error = Some("writer channel closed without completion".into());
    let discarded = state.current.discarded_bytes;
    state.catalog.record(CatalogEvent::Incomplete {
        discarded_buffered_bytes: discarded,
        discarded_bytes_known: false,
        reason: "writer channel closed",
    })?;
    state.publish();
    Ok(())
}

fn append_batch(
    state: &mut WriterState,
    batch: &mut Vec<RawRecord>,
    config: &RuntimeConfig,
) -> Result<(), RuntimeError> {
    if batch.is_empty() {
        return Ok(());
    }
    if !config.writer_delay.is_zero() {
        std::thread::sleep(config.writer_delay);
    }
    let mut records = std::mem::take(batch).into_iter();
    let mut appended = 0_u64;
    while let Some(record) = records.next() {
        let estimated = record.bytes.len() as u64 + record.delimiter.len() as u64 + 80;
        if let Some(limit) = config.storage_limit_bytes
            && state.current.journal_bytes.saturating_add(estimated) > limit
        {
            let rejected_bytes = record.bytes.len() as u64
                + record.delimiter.len() as u64
                + records
                    .as_slice()
                    .iter()
                    .map(|pending| (pending.bytes.len() + pending.delimiter.len()) as u64)
                    .sum::<u64>();
            state.current.state = RuntimeState::StorageBlocked;
            state.current.discarded_bytes =
                state.current.discarded_bytes.saturating_add(rejected_bytes);
            state.current.discarded_bytes_known = false;
            state.current.last_error = Some(format!("durable capture limit {limit} bytes reached"));
            let discarded = state.current.discarded_bytes;
            state.catalog.record(CatalogEvent::StorageBlocked {
                limit_bytes: limit,
                discarded_buffered_bytes: discarded,
                discarded_bytes_known: false,
            })?;
            state.commit.appended(appended);
            state.publish();
            return Err(RuntimeError::StorageLimit { limit });
        }
        let id = state.journal.append(record)?;
        state.current.records += 1;
        appended += 1;
        state.current.high_watermark = Some(id);
        // The journal knows how much it has written; asking the filesystem
        // once per record was a `stat` for every line of every log.
        state.current.journal_bytes = state.journal.written_bytes();
    }
    state.journal.flush()?;
    state.commit.appended(appended);
    state.commit_if_due()?;
    let externally_visible = state.progress.borrow().state;
    if matches!(
        externally_visible,
        RuntimeState::Stopping | RuntimeState::Aborting
    ) {
        state.current.state = externally_visible;
    }
    state.publish();
    Ok(())
}

fn handle_non_record(
    state: &mut WriterState,
    message: WriterMessage,
    config: &RuntimeConfig,
) -> Result<bool, RuntimeError> {
    match message {
        WriterMessage::Event { event, .. } => {
            match event {
                CaptureEvent::Boundary {
                    acquisition_id,
                    reason,
                } => {
                    state.current.boundaries += 1;
                    state.catalog.record(CatalogEvent::Boundary {
                        acquisition_id,
                        reason: boundary_name(reason),
                    })?;
                }
                CaptureEvent::CommandExit {
                    acquisition_id,
                    status,
                } => {
                    state.current.exit_code = status.code();
                    state.catalog.record(CatalogEvent::CommandExit {
                        acquisition_id,
                        code: status.code(),
                        success: status.success(),
                    })?;
                }
                CaptureEvent::Error {
                    acquisition_id,
                    message,
                } => {
                    state.current.last_error = Some(message.clone());
                    state.catalog.record(CatalogEvent::Error {
                        acquisition_id,
                        message: &message,
                    })?;
                }
                CaptureEvent::FileCheckpoint {
                    acquisition_id,
                    cursor: checkpoint,
                    encoding,
                } => {
                    let cursor = state.file_cursor.as_ref().ok_or_else(|| {
                        RuntimeError::Io(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "file checkpoint received for non-file source",
                        ))
                    })?;
                    // Not an fsync. The offset is taken now, so any later
                    // commit covers these records; the cursor is written by
                    // that commit and never before it.
                    let durable = DurableFileCursor {
                        schema_version: 1,
                        source_id: state.current.source_id,
                        path: cursor.source_path.clone(),
                        acquisition_id,
                        journal_offset: state.journal.end_offset()?,
                        file: checkpoint,
                        encoding,
                    };
                    let cursor_path = cursor.cursor_path.clone();
                    state.commit.defer_cursor(cursor_path, durable);
                }
                CaptureEvent::Stopped { .. } | CaptureEvent::Records(_) => {}
            }
            state.publish();
            Ok(false)
        }
        WriterMessage::Page {
            offset,
            max_records,
            max_bytes,
            reply,
            ..
        } => {
            let result = state
                .journal
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
            state: requested,
            discarded_bytes,
            discarded_bytes_known,
            reply,
        } => {
            let requested = if reply.is_closed() {
                RuntimeState::Incomplete
            } else {
                requested
            };
            let result = finish_writer(state, requested, discarded_bytes, discarded_bytes_known);
            let successful = result.is_ok();
            if successful {
                // Publish durable counters, but leave terminal-state ownership
                // to the supervisor after it joins this writer and releases the
                // runtime lease. Publishing `current.state` here permits reopen
                // before cleanup has finished.
                let mut durable = state.current.clone();
                durable.state = state.progress.borrow().state;
                let _ = state.progress.send(durable);
            }
            let _ = reply.send(result);
            Ok(successful)
        }
    }
}

fn finish_writer(
    state: &mut WriterState,
    requested: RuntimeState,
    discarded_bytes: u64,
    discarded_bytes_known: bool,
) -> Result<(), RuntimeError> {
    // Stopping is the one moment durability is unconditional: everything
    // captured is committed and the held cursor is written behind it, so a
    // clean stop never leaves work for a restart to re-read.
    state.commit()?;
    state.current.journal_bytes = state.journal.written_bytes();
    state.current.state = requested;
    state.current.discarded_bytes = discarded_bytes;
    state.current.discarded_bytes_known = discarded_bytes_known;
    match requested {
        RuntimeState::Stopped => state.catalog.record(CatalogEvent::Stopped)?,
        RuntimeState::Aborted => state.catalog.record(CatalogEvent::Aborted {
            discarded_buffered_bytes: discarded_bytes,
            discarded_bytes_known,
        })?,
        RuntimeState::Incomplete => state.catalog.record(CatalogEvent::Incomplete {
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
