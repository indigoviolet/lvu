use crate::{
    catalog::{Catalog, CatalogEvent},
    manager::{RuntimeConfig, RuntimeError, RuntimeState, SourceProgress},
};
use lvu_core::{
    CaptureEvent, Journal, JournalPage, RawRecord, RecordId, SourceId, acquisition::BoundaryReason,
};
use std::{fs, path::PathBuf, sync::Arc};
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
}

pub(crate) async fn spawn_writer(
    source_id: SourceId,
    journal_path: PathBuf,
    catalog_path: PathBuf,
    config: RuntimeConfig,
    progress: watch::Sender<SourceProgress>,
    mut initial: SourceProgress,
) -> Result<WriterInit, RuntimeError> {
    let open_path = journal_path.clone();
    let (journal, recovery, mut catalog) = tokio::task::spawn_blocking(move || {
        let (journal, recovery) = Journal::open(&open_path, source_id)?;
        let catalog = Catalog::open(&catalog_path)?;
        Ok::<_, RuntimeError>((journal, recovery, catalog))
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
    let task = tokio::task::spawn_blocking(move || {
        if let Err(error) = run_writer(
            journal,
            catalog,
            config,
            progress.clone(),
            initial,
            receiver,
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
    })
}

fn run_writer(
    mut journal: Journal,
    mut catalog: Catalog,
    config: RuntimeConfig,
    progress: watch::Sender<SourceProgress>,
    mut current: SourceProgress,
    mut receiver: tokio::sync::mpsc::Receiver<WriterMessage>,
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
                let _ = progress.send(current.clone());
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
