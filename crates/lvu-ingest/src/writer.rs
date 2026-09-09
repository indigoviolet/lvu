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
    Finish {
        state: RuntimeState,
        discarded_bytes: u64,
        discarded_bytes_known: bool,
        reply: oneshot::Sender<Result<(), RuntimeError>>,
    },
}

/// A bounded journal page read for live indexing and query workers.
///
/// Page reads never append, reorder capture events among themselves, change
/// append durability, or move the file cursor, which still advances only
/// behind the commit that covers it. They carry no capture payload, so they
/// need no writer-queue slot: the per-source page gate already bounds them to
/// one outstanding request, and the page channel below bounds them to one
/// queued message.
///
/// The gate permit travels inside the request until service or drop, rather
/// than staying with the calling future: a caller cancelled after sending
/// must not free the gate while its request is still queued or being served,
/// or a replacement caller could pass and break the one-outstanding bound.
pub(crate) struct PageRequest {
    pub offset: u64,
    pub max_records: usize,
    pub max_bytes: usize,
    pub reply: oneshot::Sender<Result<JournalPage, RuntimeError>>,
    pub _permit: OwnedSemaphorePermit,
}

/// At most one outstanding page per source (enforced by the page gate), so a
/// capacity of one never blocks a sender and the writer's inbound stays
/// bounded at `writer_queue_capacity` events plus this single page.
const PAGE_CHANNEL_CAPACITY: usize = 1;

pub(crate) struct WriterInit {
    pub sender: tokio::sync::mpsc::Sender<WriterMessage>,
    pub page_sender: tokio::sync::mpsc::Sender<PageRequest>,
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
    let (sender, event_receiver) = tokio::sync::mpsc::channel(capacity);
    let (page_sender, page_receiver) = tokio::sync::mpsc::channel(PAGE_CHANNEL_CAPACITY);
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
        let outcome = run_writer(state, config, event_receiver, page_receiver, runtime);
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
        page_sender,
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

    /// Adds one capture-only interval. Page reads share this writer thread but
    /// belong to live indexing/query work, so callers deliberately start a new
    /// interval around each message and mark page service uncharged.
    fn note_cpu(&mut self, cpu: &lvu_core::ThreadCpu, capture_work: bool) {
        self.current.writer_cpu_nanos = accumulated_capture_cpu(
            self.current.writer_cpu_nanos,
            capture_work,
            cpu.elapsed_nanos(),
        );
    }
}

fn accumulated_capture_cpu(total: u64, capture_work: bool, elapsed: u64) -> u64 {
    if capture_work {
        total.saturating_add(elapsed)
    } else {
        total
    }
}

/// What the writer wait woke for.
///
/// Pages have their own bounded channel so a flooded capture queue cannot
/// refuse a page admission, and the two sides strictly alternate while both
/// have work queued: a page reads committed records the writer has already
/// published, never appends, and never moves the file cursor, so serving it
/// between capture batches does not reorder capture events among themselves,
/// change what becomes durable, or unveil uncommitted tail to callers that
/// page from the published progress. Alternation is the reciprocal half of
/// the same guarantee: a page gate bounds occupancy to one outstanding page,
/// which bounds how many pages can be queued but not how often they arrive,
/// so pages always winning would let sustained paging stall capture.
/// Alternating keeps bounded progress for both sides: a page waits for at most
/// the batch in progress, and a capture batch waits for at most the page in
/// progress. This addresses the scheduling half of the journal/page-read TODO
/// row; what it does to any volume-backed query number is for a bounded
/// experiment to say, not claimed here.
enum Wake {
    Page(PageRequest),
    Event(WriterMessage),
    Timeout,
    EventsClosed,
    Closed,
}

/// Takes whatever is already queued without blocking.
///
/// When both channels already hold work, `prefer_pages` decides which is
/// served first and the caller flips it after every service, which is what
/// alternates the two sides under sustained load: the page needs no queued
/// capture tail, and the queued capture keeps its arrival order for the batch
/// that follows. When only one side has work it is served regardless of the
/// flag, so neither side idles. While the event channel is closed abnormally
/// only queued events are taken: queued pages fall back to read-only reads
/// once the writer below returns, which see everything the close path below
/// commits first.
fn take_queued(
    events: &mut tokio::sync::mpsc::Receiver<WriterMessage>,
    pages: &mut tokio::sync::mpsc::Receiver<PageRequest>,
    prefer_pages: bool,
) -> Option<Wake> {
    if prefer_pages {
        if let Ok(page) = pages.try_recv() {
            return Some(Wake::Page(page));
        }
        if let Ok(event) = events.try_recv() {
            return Some(Wake::Event(event));
        }
    } else {
        if let Ok(event) = events.try_recv() {
            return Some(Wake::Event(event));
        }
        if let Ok(page) = pages.try_recv() {
            return Some(Wake::Page(page));
        }
    }
    None
}

/// Waits for the next writer message, bounded by the commit deadline.
///
/// The writer is a blocking thread, so it cannot await; `block_on` here is safe
/// because this is not a runtime worker. `Timeout` means the deadline passed
/// with nothing arriving. The caller only waits here when nothing is queued,
/// so at most one side can close while waiting: `EventsClosed` means the event
/// channel closed first and the caller drains and terminates, while `Closed`
/// means both are gone. The fixed page-first tie-break only decides messages
/// arriving at the same instant; whichever side it wakes, the caller's flag
/// still flips, so the next queued turn goes to the other side.
fn blocking_wait(
    runtime: &tokio::runtime::Handle,
    events: &mut tokio::sync::mpsc::Receiver<WriterMessage>,
    pages: &mut tokio::sync::mpsc::Receiver<PageRequest>,
    deadline: Option<std::time::Duration>,
) -> Wake {
    runtime.block_on(async {
        let wait = async {
            tokio::select! {
                biased;
                page = pages.recv() => match page {
                    Some(page) => Wake::Page(page),
                    // Pages closed: events only from here.
                    None => match events.recv().await {
                        Some(event) => Wake::Event(event),
                        None => Wake::Closed,
                    },
                },
                event = events.recv() => match event {
                    Some(event) => Wake::Event(event),
                    // Events closed mid-wait: the caller serves what is queued
                    // and then terminates instead of waiting on pages.
                    None => Wake::EventsClosed,
                },
            }
        };
        match deadline {
            None => wait.await,
            Some(remaining) => match tokio::time::timeout(remaining, wait).await {
                Ok(wake) => wake,
                Err(_) => Wake::Timeout,
            },
        }
    })
}

fn run_writer(
    mut state: WriterState,
    config: RuntimeConfig,
    mut events: tokio::sync::mpsc::Receiver<WriterMessage>,
    mut pages: tokio::sync::mpsc::Receiver<PageRequest>,
    runtime: tokio::runtime::Handle,
) -> Result<(), RuntimeError> {
    let mut batch = Vec::with_capacity(config.batch_records);
    // Pages start preferred so a page queued behind capture at startup is
    // still served between batches rather than behind the whole queue.
    let mut prefer_pages = true;
    // The time half of group commit has to hold when nothing is arriving: a
    // tail that goes quiet must still become durable, and the cursor it is
    // holding must still be written. So the wait for the next message is
    // bounded by the commit deadline whenever there is anything outstanding,
    // and unbounded when there is not.
    loop {
        // A closed and drained event channel ends the writer even while page
        // senders are retained: no Finish can arrive anymore. This check runs
        // before page service on every turn, so a continuously replenished
        // page fast path can never hide the closure and stall the writer
        // join (and the source lifecycle it gates) forever. Queued events —
        // including any queued Finish — keep the buffer non-empty, so they
        // are drained in order first and a pending Finish still terminates
        // cleanly; queued pages were served alongside them by the fast path
        // below. Pages sent from here race our return and fall back to
        // read-only reads over everything committed below. Both predicates
        // are monotonic once observed together: with no senders left, nothing
        // can enqueue anymore.
        if events.is_closed() && events.is_empty() {
            break;
        }
        // An overdue group commit outranks everything queued. The fast path
        // below would otherwise serve continuous paging forever without ever
        // reaching the deadline wait, leaving appended records undurable past
        // their time bound. Count-based commits still happen in the batch
        // path; this only fires once the time bound has actually expired.
        if state.commit.due() {
            let writer_cpu = lvu_core::ThreadCpu::start();
            state.commit()?;
            state.note_cpu(&writer_cpu, true);
            state.publish();
        }
        if let Some(wake) = take_queued(&mut events, &mut pages, prefer_pages) {
            match wake {
                Wake::Page(page) => {
                    let writer_cpu = lvu_core::ThreadCpu::start();
                    serve_page(&mut state, page, &config, &writer_cpu);
                    // Yield the next queued turn to capture: sustained paging
                    // must not stall appends.
                    prefer_pages = false;
                    continue;
                }
                Wake::Event(message) => {
                    let mut writer_cpu = lvu_core::ThreadCpu::start();
                    match message {
                        WriterMessage::Event {
                            event: CaptureEvent::Records(records),
                            ..
                        } => {
                            if append_records(
                                &mut state,
                                &mut events,
                                &mut batch,
                                &config,
                                records,
                                &mut writer_cpu,
                            )? {
                                return Ok(());
                            }
                        }
                        other => {
                            if handle_non_record(&mut state, other, &config, &writer_cpu)? {
                                return Ok(());
                            }
                        }
                    }
                    // Yield the next queued turn to reads: queued capture
                    // keeps its arrival order, but a waiting page goes next.
                    prefer_pages = true;
                    continue;
                }
                Wake::Timeout | Wake::EventsClosed | Wake::Closed => {
                    unreachable!("take_queued serves queued work only")
                }
            }
        }
        // Nothing queued. An event channel observed closed above already broke
        // out; reaching here means it is still open, so waiting is safe.
        match blocking_wait(&runtime, &mut events, &mut pages, state.commit.deadline()) {
            Wake::Closed => break,
            Wake::EventsClosed => {
                // Loops back to the closed-and-drained check above, which now
                // terminates since the event side proved drained.
                continue;
            }
            Wake::Timeout => {
                if state.commit.outstanding() {
                    let writer_cpu = lvu_core::ThreadCpu::start();
                    state.commit()?;
                    state.note_cpu(&writer_cpu, true);
                    state.publish();
                }
                continue;
            }
            Wake::Page(page) => {
                let writer_cpu = lvu_core::ThreadCpu::start();
                serve_page(&mut state, page, &config, &writer_cpu);
                prefer_pages = false;
                continue;
            }
            Wake::Event(message) => {
                let mut writer_cpu = lvu_core::ThreadCpu::start();
                match message {
                    WriterMessage::Event {
                        event: CaptureEvent::Records(records),
                        ..
                    } => {
                        if append_records(
                            &mut state,
                            &mut events,
                            &mut batch,
                            &config,
                            records,
                            &mut writer_cpu,
                        )? {
                            return Ok(());
                        }
                    }
                    other => {
                        if handle_non_record(&mut state, other, &config, &writer_cpu)? {
                            return Ok(());
                        }
                    }
                }
                prefer_pages = true;
                continue;
            }
        }
    }
    // The channels closed without a Finish: commit what is held, then say so.
    let writer_cpu = lvu_core::ThreadCpu::start();
    state.commit()?;
    state.note_cpu(&writer_cpu, true);
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

/// Batches one capture handover with whatever record events are already queued
/// on the event channel, preserving their arrival order, then appends once.
///
/// Only the event channel is drained here: pages arriving mid-batch wait in
/// their own channel for the batch in progress, never for every batch already
/// queued. Non-record events still split the batch so checkpoints keep the
/// journal offset of the records that preceded them. Returns true when a
/// terminal Finish was consumed and the writer must return.
fn append_records(
    state: &mut WriterState,
    events: &mut tokio::sync::mpsc::Receiver<WriterMessage>,
    batch: &mut Vec<RawRecord>,
    config: &RuntimeConfig,
    records: Vec<lvu_core::acquisition::CapturedRecord>,
    writer_cpu: &mut lvu_core::ThreadCpu,
) -> Result<bool, RuntimeError> {
    let source_id = state.current.source_id;
    state.current.handovers += 1;
    state.current.reader_cpu_nanos = state.current.reader_cpu_nanos.saturating_add(
        records
            .iter()
            .map(|record| record.reader_cpu_nanos)
            .sum::<u64>(),
    );
    batch.extend(records.into_iter().map(|record| record.into_raw(source_id)));
    while batch.len() < config.batch_records {
        match events.try_recv() {
            Ok(WriterMessage::Event {
                event: CaptureEvent::Records(records),
                ..
            }) => {
                state.current.handovers += 1;
                state.current.reader_cpu_nanos = state.current.reader_cpu_nanos.saturating_add(
                    records
                        .iter()
                        .map(|record| record.reader_cpu_nanos)
                        .sum::<u64>(),
                );
                batch.extend(records.into_iter().map(|record| record.into_raw(source_id)));
            }
            Ok(other) => {
                append_batch(state, batch, config, writer_cpu)?;
                *writer_cpu = lvu_core::ThreadCpu::start();
                if handle_non_record(state, other, config, writer_cpu)? {
                    return Ok(true);
                }
                break;
            }
            Err(_) => break,
        }
    }
    append_batch(state, batch, config, writer_cpu)?;
    Ok(false)
}

fn append_batch(
    state: &mut WriterState,
    batch: &mut Vec<RawRecord>,
    config: &RuntimeConfig,
    cpu: &lvu_core::ThreadCpu,
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
    state.note_cpu(cpu, true);
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
    _config: &RuntimeConfig,
    cpu: &lvu_core::ThreadCpu,
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
            state.note_cpu(cpu, true);
            state.publish();
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
            state.note_cpu(cpu, true);
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

/// Serves one page read from the writer-owned journal.
///
/// The journal is shared with live indexing, but its page service is not
/// capture work and must not move the capture-only counter.
fn serve_page(
    state: &mut WriterState,
    page: PageRequest,
    config: &RuntimeConfig,
    cpu: &lvu_core::ThreadCpu,
) {
    let PageRequest {
        offset,
        max_records,
        max_bytes,
        reply,
        // Held through service: the gate stays acquired until this request is
        // answered, and releases with it if the writer goes away first.
        _permit,
    } = page;
    let result = state
        .journal
        .read_page(
            offset,
            max_records.min(config.max_page_records),
            max_bytes.min(config.max_page_bytes),
        )
        .map_err(RuntimeError::from);
    state.note_cpu(cpu, false);
    let _ = reply.send(result);
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

#[cfg(test)]
mod cpu_accounting_tests {
    use super::*;
    use lvu_core::{ChunkPosition, RecordBytes, StreamKind, thread_cpu_nanos};
    use uuid::Uuid;

    #[test]
    fn journal_page_service_does_not_advance_capture_cpu_but_capture_does() {
        let directory = tempfile::tempdir().expect("temporary writer root");
        let source_id = SourceId::new();
        let (journal, _) =
            Journal::open(directory.path().join("capture.journal"), source_id).expect("journal");
        let catalog = Catalog::open(&directory.path().join("catalog.sqlite3")).expect("catalog");
        let current = SourceProgress {
            source_id,
            generation: 1,
            state: RuntimeState::Running,
            records: 0,
            high_watermark: None,
            journal_bytes: 0,
            synced_records: 0,
            syncs: 0,
            handovers: 0,
            writer_cpu_nanos: 123,
            reader_cpu_nanos: 0,
            boundaries: 0,
            exit_code: None,
            discarded_bytes: 0,
            discarded_bytes_known: true,
            last_error: None,
        };
        let (progress, _) = watch::channel(current.clone());
        let config = RuntimeConfig {
            sync_every_records: 1,
            ..RuntimeConfig::default()
        };
        let mut state = WriterState {
            journal,
            catalog,
            current,
            progress,
            file_cursor: None,
            commit: Commit::new(&config),
        };
        let (reply, receive) = oneshot::channel();
        let page_cpu = lvu_core::ThreadCpu::start();
        serve_page(
            &mut state,
            PageRequest {
                offset: 0,
                max_records: 1,
                max_bytes: 1024,
                reply,
                _permit: Arc::new(Semaphore::new(1))
                    .try_acquire_owned()
                    .expect("page permit"),
            },
            &config,
            &page_cpu,
        );
        assert!(receive.blocking_recv().expect("page reply").is_ok());
        assert_eq!(state.current.writer_cpu_nanos, 123);

        let mut batch = vec![RawRecord {
            record_id: RecordId {
                source_id,
                sequence: 0,
            },
            captured_at_unix_nanos: 1,
            stream: StreamKind::File,
            bytes: RecordBytes::from(b"captured after page service"),
            delimiter: RecordBytes::from(b"\n"),
            acquisition_id: Uuid::new_v4(),
            chunk: ChunkPosition::Complete,
        }];
        let capture_cpu = lvu_core::ThreadCpu::start();
        append_batch(&mut state, &mut batch, &config, &capture_cpu)
            .expect("append and commit capture batch");
        assert!(batch.is_empty());
        assert_eq!(state.current.records, 1);
        assert_eq!(state.current.synced_records, 1);
        if thread_cpu_nanos().is_some() {
            assert!(state.current.writer_cpu_nanos > 123);
        }
    }
}

#[cfg(test)]
mod scheduling_tests {
    //! Ordering proof for the split page/event schedule, without any
    //! wall-clock oracle.
    //!
    //! Every test below stages its whole input in bounded channels *before*
    //! the writer thread starts, so what the writer sees first is fixed and no
    //! assertion measures time. Generous timeouts guard against a hang only;
    //! none of them decides pass or fail. `writer_delay` stays zero throughout:
    //! nothing here needs the writer to be slow, because the schedule, not the
    //! speed, is under test.
    //!
    //! On the old single shared queue these same stages serve strictly FIFO,
    //! so the overtake assertions fail there by construction while the
    //! preservation assertions (order, bytes, boundaries, durability) hold on
    //! both: reordering pages before queued capture must never reorder capture
    //! itself.

    use super::*;
    use lvu_core::{ChunkPosition, JournalReader, RecordBytes, StreamKind};
    use std::time::Duration;
    use uuid::Uuid;

    fn captured(tag: &str, acquisition_id: Uuid) -> lvu_core::acquisition::CapturedRecord {
        lvu_core::acquisition::CapturedRecord {
            captured_at_unix_nanos: 1,
            stream: StreamKind::File,
            bytes: RecordBytes::from(tag.as_bytes()),
            delimiter: RecordBytes::from(b"\n"),
            acquisition_id,
            chunk: ChunkPosition::Complete,
            reader_cpu_nanos: 0,
        }
    }

    fn event(
        slots: &Arc<Semaphore>,
        records: Vec<lvu_core::acquisition::CapturedRecord>,
    ) -> WriterMessage {
        WriterMessage::Event {
            event: CaptureEvent::Records(records),
            _permit: slots
                .clone()
                .try_acquire_owned()
                .expect("test event permit"),
        }
    }

    /// Builds a page request holding its own gate permit, exactly as
    /// `SourceHandle::read_page` admits one: the permit travels with the
    /// request so cancelling the caller cannot free the gate early.
    fn test_page(
        gate: &Arc<Semaphore>,
        offset: u64,
        max_records: usize,
        max_bytes: usize,
    ) -> (
        PageRequest,
        oneshot::Receiver<Result<JournalPage, RuntimeError>>,
    ) {
        let (reply, received) = oneshot::channel();
        let request = PageRequest {
            offset,
            max_records,
            max_bytes,
            reply,
            _permit: gate.clone().try_acquire_owned().expect("test page permit"),
        };
        (request, received)
    }

    struct Fixture {
        // Owns the temporary root: dropping it would delete the journal and
        // catalog under test, so the field is lifetime, not data.
        #[allow(dead_code)]
        directory: tempfile::TempDir,
        journal_path: PathBuf,
        catalog_path: PathBuf,
        source_id: SourceId,
        progress_rx: watch::Receiver<SourceProgress>,
        state: Option<WriterState>,
        config: RuntimeConfig,
    }

    impl Fixture {
        fn new() -> Self {
            Self::with_sync_interval(Duration::from_secs(3600))
        }

        fn with_sync_interval(sync_interval: Duration) -> Self {
            let directory = tempfile::tempdir().expect("temporary writer root");
            let journal_path = directory.path().join("capture.journal");
            let catalog_path = directory.path().join("catalog.sqlite3");
            let source_id = SourceId::new();
            let (journal, _) = Journal::open(&journal_path, source_id).expect("journal");
            let catalog = Catalog::open(&catalog_path).expect("catalog");
            let current = SourceProgress {
                source_id,
                generation: 1,
                state: RuntimeState::Running,
                records: 0,
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
            };
            let (progress_tx, progress_rx) = watch::channel(current.clone());
            let config = RuntimeConfig {
                sync_every_records: u64::MAX,
                sync_interval,
                batch_records: 1,
                ..RuntimeConfig::default()
            };
            let state = WriterState {
                journal,
                catalog,
                current,
                progress: progress_tx,
                file_cursor: None,
                commit: Commit::new(&config),
            };
            Self {
                directory,
                journal_path,
                catalog_path,
                source_id,
                progress_rx,
                state: Some(state),
                config,
            }
        }

        fn replay(&self) -> Vec<u8> {
            let mut reader =
                JournalReader::open(&self.journal_path, self.source_id).expect("reader");
            let mut offset = 0;
            let mut replayed = Vec::new();
            loop {
                let page = reader
                    .read_page(offset, 64, 1024 * 1024)
                    .expect("replay page");
                for record in &page.records {
                    replayed.extend_from_slice(&record.bytes);
                    replayed.extend_from_slice(&record.delimiter);
                }
                if page.end_of_journal {
                    return replayed;
                }
                offset = page.next_offset;
            }
        }
    }

    async fn finish(
        event_tx: &tokio::sync::mpsc::Sender<WriterMessage>,
    ) -> Result<(), RuntimeError> {
        let (reply, receive) = oneshot::channel();
        event_tx
            .send(WriterMessage::Finish {
                state: RuntimeState::Stopped,
                discarded_bytes: 0,
                discarded_bytes_known: true,
                reply,
            })
            .await
            .expect("finish sent");
        receive.await.expect("finish reply")?;
        Ok(())
    }

    /// A page staged behind queued capture is served before that capture,
    /// while capture itself keeps its arrival order, boundaries, bytes and
    /// durability. FIFO service would append everything first, so the page
    /// would carry all six records instead of none.
    #[tokio::test]
    async fn queued_page_overtakes_queued_batches_without_reordering_capture() {
        let mut fixture = Fixture::new();
        let slots = Arc::new(Semaphore::new(16));
        let (event_tx, event_rx) = tokio::sync::mpsc::channel(8);
        let (page_tx, page_rx) = tokio::sync::mpsc::channel(4);
        let acquisition_id = Uuid::new_v4();
        let body = |tag: &str| captured(tag, acquisition_id);

        // Staged before the writer exists: E1, a boundary, E2, E3, then the
        // page. Both channels are non-empty at the writer's first wait, so the
        // initial page preference decides deterministically.
        event_tx
            .send(event(&slots, vec![body("e1a"), body("e1b")]))
            .await
            .expect("E1 staged");
        event_tx
            .send(WriterMessage::Event {
                event: CaptureEvent::Boundary {
                    acquisition_id,
                    reason: BoundaryReason::Started,
                },
                _permit: slots.clone().try_acquire_owned().expect("boundary permit"),
            })
            .await
            .expect("boundary staged");
        event_tx
            .send(event(&slots, vec![body("e2a"), body("e2b")]))
            .await
            .expect("E2 staged");
        event_tx
            .send(event(&slots, vec![body("e3a"), body("e3b")]))
            .await
            .expect("E3 staged");
        let gate = Arc::new(Semaphore::new(8));
        let (page_request, page_received) = test_page(&gate, 0, 100, 1024 * 1024);
        page_tx.send(page_request).await.expect("page staged");

        let runtime = tokio::runtime::Handle::current();
        let state = fixture.state.take().expect("writer state");
        let config = fixture.config.clone();
        let writer = tokio::task::spawn_blocking(move || {
            run_writer(state, config, event_rx, page_rx, runtime)
        });

        // The page overtook every queued batch: nothing was committed when it
        // was staged, and the writer serves it before appending any of them.
        let page = tokio::time::timeout(Duration::from_secs(30), page_received)
            .await
            .expect("page answered")
            .expect("page reply")
            .expect("page read");
        assert!(
            page.records.is_empty(),
            "queued page must read the committed prefix, not the queued tail"
        );

        finish(&event_tx).await.expect("clean stop");
        tokio::time::timeout(Duration::from_secs(30), writer)
            .await
            .expect("writer joined")
            .expect("writer task")
            .expect("writer ok");

        let progress = fixture.progress_rx.borrow().clone();
        assert_eq!(progress.records, 6);
        assert_eq!(progress.handovers, 3);
        assert_eq!(progress.boundaries, 1);
        assert_eq!(progress.synced_records, 6);
        assert_eq!(
            fixture.replay(),
            b"e1a\ne1b\ne2a\ne2b\ne3a\ne3b\n".as_slice(),
            "capture order and bytes survive the overtake"
        );
    }

    /// Sustained pages cannot stall capture: with an event and two pages
    /// staged, service alternates page, event, page. Page monopolization would
    /// serve both pages before the event, leaving the second page empty.
    #[tokio::test]
    async fn sustained_pages_alternate_with_capture_so_both_progress() {
        let mut fixture = Fixture::new();
        let slots = Arc::new(Semaphore::new(16));
        let (event_tx, event_rx) = tokio::sync::mpsc::channel(8);
        let (page_tx, page_rx) = tokio::sync::mpsc::channel(4);
        let acquisition_id = Uuid::new_v4();

        event_tx
            .send(event(
                &slots,
                vec![
                    captured("e1a", acquisition_id),
                    captured("e1b", acquisition_id),
                ],
            ))
            .await
            .expect("event staged");
        let gate = Arc::new(Semaphore::new(8));
        let (first_request, first_received) = test_page(&gate, 0, 10, 1024 * 1024);
        page_tx
            .send(first_request)
            .await
            .expect("first page staged");
        let (second_request, second_received) = test_page(&gate, 0, 10, 1024 * 1024);
        page_tx
            .send(second_request)
            .await
            .expect("second page staged");

        let runtime = tokio::runtime::Handle::current();
        let state = fixture.state.take().expect("writer state");
        let config = fixture.config.clone();
        let writer = tokio::task::spawn_blocking(move || {
            run_writer(state, config, event_rx, page_rx, runtime)
        });

        // First the waiting page. Its content proves the order: the reply
        // carries what the journal held when the page was served, so an empty
        // page means it ran before any append regardless of how the threads
        // race afterwards.
        let first = tokio::time::timeout(Duration::from_secs(30), first_received)
            .await
            .expect("first page answered")
            .expect("page reply")
            .expect("page read");
        assert!(first.records.is_empty());

        // Then the queued capture before the second page: the event was not
        // starved behind two pages.
        let second = tokio::time::timeout(Duration::from_secs(30), second_received)
            .await
            .expect("second page answered")
            .expect("page reply")
            .expect("page read");
        assert_eq!(second.records.len(), 2);
        assert_eq!(second.records[0].bytes.as_slice(), b"e1a".as_slice());

        finish(&event_tx).await.expect("clean stop");
        tokio::time::timeout(Duration::from_secs(30), writer)
            .await
            .expect("writer joined")
            .expect("writer task")
            .expect("writer ok");
        assert_eq!(fixture.progress_rx.borrow().records, 2);
        assert_eq!(
            fixture.replay(),
            b"e1a\ne1b\n".as_slice(),
            "capture order and bytes survive alternation"
        );
    }

    /// A pending page never blocks termination: with a page and a Finish
    /// staged together, the writer answers the page and still returns, and the
    /// journal stays readable through the read-only path afterwards.
    #[tokio::test]
    async fn pending_page_does_not_block_finish_and_stays_readable() {
        let mut fixture = Fixture::new();
        let (event_tx, event_rx) = tokio::sync::mpsc::channel(8);
        let (page_tx, page_rx) = tokio::sync::mpsc::channel(4);
        let gate = Arc::new(Semaphore::new(8));

        let (page_request, page_received) = test_page(&gate, 0, 10, 1024 * 1024);
        page_tx.send(page_request).await.expect("page staged");
        let (finish_reply, finish_received) = oneshot::channel();
        event_tx
            .send(WriterMessage::Finish {
                state: RuntimeState::Stopped,
                discarded_bytes: 0,
                discarded_bytes_known: true,
                reply: finish_reply,
            })
            .await
            .expect("finish staged");

        let runtime = tokio::runtime::Handle::current();
        let state = fixture.state.take().expect("writer state");
        let config = fixture.config.clone();
        let writer = tokio::task::spawn_blocking(move || {
            run_writer(state, config, event_rx, page_rx, runtime)
        });

        let page = tokio::time::timeout(Duration::from_secs(30), page_received)
            .await
            .expect("page answered")
            .expect("page reply")
            .expect("page read");
        assert!(page.records.is_empty());
        tokio::time::timeout(Duration::from_secs(30), finish_received)
            .await
            .expect("finish answered")
            .expect("finish reply")
            .expect("finish ok");
        tokio::time::timeout(Duration::from_secs(30), writer)
            .await
            .expect("writer joined")
            .expect("writer task")
            .expect("writer ok");

        // Post-close reads use the read-only journal path, as the manager's
        // fallback does once the writer is gone.
        let mut reader =
            JournalReader::open(&fixture.journal_path, fixture.source_id).expect("reader");
        let page = reader
            .read_page(0, 10, 1024 * 1024)
            .expect("post-close read");
        assert!(page.records.is_empty());
        assert!(page.end_of_journal);
    }

    /// An overdue time commit fires before queued page service, not after the
    /// pages drain. The fast path would otherwise serve paging forever without
    /// ever reaching the deadline wait, leaving appended records undurable
    /// past their time bound.
    ///
    /// No assertion measures time and no flood is needed: the test backdates
    /// the commit clock past the interval with a record outstanding before the
    /// writer starts, so the very first loop turn is deterministically overdue
    /// and the verdict (synced progress already published when the first page
    /// resolves) is pure order. Without the loop-top priority the page would
    /// resolve first with nothing synced.
    #[tokio::test]
    async fn overdue_time_commit_precedes_queued_page_service() {
        let mut fixture = Fixture::with_sync_interval(Duration::from_secs(60));
        let (event_tx, event_rx) = tokio::sync::mpsc::channel(8);
        let (page_tx, page_rx) = tokio::sync::mpsc::channel(4);
        let acquisition_id = Uuid::new_v4();

        // One outstanding record appended synchronously up front: the batch
        // path cannot commit it because the interval is fresh, so exactly one
        // durable commit is owed from here on.
        let mut state = fixture.state.take().expect("writer state");
        let mut batch = vec![captured("x", acquisition_id).into_raw(fixture.source_id)];
        let cpu = lvu_core::ThreadCpu::start();
        append_batch(&mut state, &mut batch, &fixture.config, &cpu)
            .expect("stage outstanding record");
        assert!(batch.is_empty());
        assert_eq!(state.current.records, 1);
        assert_eq!(state.current.synced_records, 0);
        // Backdate past the interval: deterministically overdue before spawn.
        // The 61-second backdate needs a host clock older than a minute, which
        // any machine running a test suite satisfies; nothing here measures
        // how long anything takes.
        state.commit.last = std::time::Instant::now()
            .checked_sub(Duration::from_secs(61))
            .expect("backdate within clock range");

        let gate = Arc::new(Semaphore::new(8));
        let (page_request, page_received) = test_page(&gate, 0, 1, 1024);
        page_tx.send(page_request).await.expect("page staged");

        let runtime = tokio::runtime::Handle::current();
        let config = fixture.config.clone();
        let writer = tokio::task::spawn_blocking(move || {
            run_writer(state, config, event_rx, page_rx, runtime)
        });

        // The commit published before the page was served: single-threaded
        // program order on the writer, observed here through the reply that
        // can only arrive afterwards.
        let page = tokio::time::timeout(Duration::from_secs(30), page_received)
            .await
            .expect("page answered")
            .expect("page reply")
            .expect("page read");
        assert_eq!(page.records.len(), 1);
        assert_eq!(page.records[0].bytes.as_slice(), b"x".as_slice());
        let progress = fixture.progress_rx.borrow().clone();
        assert_eq!(progress.syncs, 1);
        assert_eq!(progress.synced_records, 1);

        finish(&event_tx).await.expect("clean stop");
        tokio::time::timeout(Duration::from_secs(30), writer)
            .await
            .expect("writer joined")
            .expect("writer task")
            .expect("writer ok");
        assert_eq!(fixture.replay(), b"x\n".as_slice());
    }

    /// An unexpected event-channel close terminates the writer instead of
    /// waiting on retained page senders, after draining queued capture.
    /// Pages sent later fail fast to the manager's read-only fallback over
    /// everything committed here.
    #[tokio::test]
    async fn unexpected_event_close_terminates_after_draining_queued_capture() {
        let mut fixture = Fixture::new();
        let slots = Arc::new(Semaphore::new(16));
        let (event_tx, event_rx) = tokio::sync::mpsc::channel(8);
        let (page_tx, page_rx) = tokio::sync::mpsc::channel(4);
        let acquisition_id = Uuid::new_v4();

        event_tx
            .send(event(&slots, vec![captured("e1a", acquisition_id)]))
            .await
            .expect("event staged");
        let gate = Arc::new(Semaphore::new(8));
        let (page_request, page_received) = test_page(&gate, 0, 10, 1024 * 1024);
        page_tx.send(page_request).await.expect("page staged");
        // Abnormal end: no Finish will ever arrive, while the page sender is
        // retained as live handles would retain it.
        drop(event_tx);

        let runtime = tokio::runtime::Handle::current();
        let state = fixture.state.take().expect("writer state");
        let config = fixture.config.clone();
        let writer = tokio::task::spawn_blocking(move || {
            run_writer(state, config, event_rx, page_rx, runtime)
        });

        // The queued page is still answered from the committed prefix.
        let page = tokio::time::timeout(Duration::from_secs(30), page_received)
            .await
            .expect("page answered")
            .expect("page reply")
            .expect("page read");
        assert!(page.records.is_empty());
        // Then the writer terminates instead of waiting on the retained page
        // sender: this join would hang under wait-on-pages-forever semantics.
        tokio::time::timeout(Duration::from_secs(30), writer)
            .await
            .expect("writer joined")
            .expect("writer task")
            .expect("writer ok");

        let progress = fixture.progress_rx.borrow().clone();
        assert_eq!(progress.records, 1);
        assert_eq!(progress.synced_records, 1);
        assert_eq!(progress.state, RuntimeState::Incomplete);
        assert_eq!(fixture.replay(), b"e1a\n".as_slice());

        // Later pages fail fast instead of hanging: the manager maps this to
        // its read-only journal fallback.
        let gate = Arc::new(Semaphore::new(8));
        let (late_request, _) = test_page(&gate, 0, 10, 1024);
        assert!(
            page_tx.send(late_request).await.is_err(),
            "pages sent after close must fail fast to the fallback, not hang"
        );
    }

    /// A Finish queued before the event channel closed still terminates
    /// normally: draining takes queued events first, so the close break below
    /// never discards a pending Finish as an Incomplete.
    #[tokio::test]
    async fn queued_finish_under_closed_events_still_stops_cleanly() {
        let mut fixture = Fixture::new();
        let slots = Arc::new(Semaphore::new(16));
        let (event_tx, event_rx) = tokio::sync::mpsc::channel(8);
        let (page_tx, page_rx) = tokio::sync::mpsc::channel(4);
        let acquisition_id = Uuid::new_v4();

        event_tx
            .send(event(&slots, vec![captured("e1a", acquisition_id)]))
            .await
            .expect("event staged");
        let gate = Arc::new(Semaphore::new(8));
        let (page_request, page_received) = test_page(&gate, 0, 10, 1024 * 1024);
        page_tx.send(page_request).await.expect("page staged");
        let (finish_reply, finish_received) = oneshot::channel();
        event_tx
            .send(WriterMessage::Finish {
                state: RuntimeState::Stopped,
                discarded_bytes: 0,
                discarded_bytes_known: true,
                reply: finish_reply,
            })
            .await
            .expect("finish staged");
        drop(event_tx);

        let runtime = tokio::runtime::Handle::current();
        let state = fixture.state.take().expect("writer state");
        let config = fixture.config.clone();
        let writer = tokio::task::spawn_blocking(move || {
            run_writer(state, config, event_rx, page_rx, runtime)
        });

        // Alternation serves the queued page, then the queued event, then the
        // queued Finish — in that order, all before any close break.
        let page = tokio::time::timeout(Duration::from_secs(30), page_received)
            .await
            .expect("page answered")
            .expect("page reply")
            .expect("page read");
        assert!(page.records.is_empty());
        tokio::time::timeout(Duration::from_secs(30), finish_received)
            .await
            .expect("finish answered")
            .expect("finish reply")
            .expect("finish ok");
        tokio::time::timeout(Duration::from_secs(30), writer)
            .await
            .expect("writer joined")
            .expect("writer task")
            .expect("writer ok");

        let progress = fixture.progress_rx.borrow().clone();
        assert_eq!(progress.records, 1);
        assert_eq!(progress.synced_records, 1);
        assert_eq!(fixture.replay(), b"e1a\n".as_slice());
        // The queued Finish took the normal terminal path, not the close
        // path: the catalog records a clean stop, never an incomplete.
        // (Terminal publication itself stays with the supervisor, so progress
        // state is not the signal here.)
        let catalog = std::fs::read_to_string(&fixture.catalog_path).expect("catalog");
        assert!(
            catalog.contains("\"event\":\"stopped\""),
            "queued Finish must record a clean stop: {catalog}"
        );
        assert!(
            !catalog.contains("incomplete"),
            "queued Finish must not take the close path: {catalog}"
        );
    }

    /// Cancelling a caller after its page is queued must not free the gate:
    /// the queued request carries its own permit, so a replacement caller
    /// cannot pass until the orphan is consumed or dropped. With the permit
    /// owned by the caller instead, dropping the cancelled future would
    /// release the gate while the orphan still occupies the channel.
    #[tokio::test]
    async fn cancelled_caller_does_not_free_the_gate_for_a_replacement() {
        let gate = Arc::new(Semaphore::new(1));
        // Never consumed: the request stays queued, exactly the state a
        // cancelled-while-queued caller leaves behind.
        let (page_tx, page_rx) = tokio::sync::mpsc::channel::<PageRequest>(4);
        // First caller admits exactly as read_page does, then is cancelled:
        // its reply handle and future are dropped with the request queued.
        let (request, reply) = test_page(&gate, 0, 1, 1024);
        drop(reply);
        page_tx.send(request).await.expect("orphan queued");
        // A replacement caller must not pass while the orphan is queued.
        assert!(
            gate.clone().try_acquire_owned().is_err(),
            "gate must stay held by the queued orphan after its caller is gone"
        );
        // Once the orphan is consumed or dropped, the gate frees.
        drop(page_rx);
        assert!(
            gate.clone().try_acquire_owned().is_ok(),
            "gate must free with the orphan"
        );
    }
}
