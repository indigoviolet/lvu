//! Durable, bounded source lifecycle history.
//!
//! Reconnects, restarts, rejections and capture gaps are facts about a capture,
//! not log records, so they are kept beside the journal rather than inside it.
//! History is bounded twice over: capture publishes into a bounded channel that
//! drops rather than blocks, and this writer retains a bounded in-memory window
//! plus a bounded on-disk file. Every bound that is reached is itself reported,
//! so history never quietly looks complete when it is not.

use lvu_core::source_event::SourceEventRecord;
use std::{
    collections::VecDeque,
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::sync::{mpsc, watch};

/// Entries retained in memory for status surfaces.
pub const HISTORY_WINDOW_ENTRIES: usize = 256;
/// Bound on the durable history file for one source.
pub const HISTORY_FILE_BYTES: u64 = 1024 * 1024;

/// An immutable view of a source's recent lifecycle history.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SourceHistory {
    /// The most recent entries, oldest first, at most `HISTORY_WINDOW_ENTRIES`.
    pub entries: VecDeque<SourceEventRecord>,
    /// Entries published since this runtime started, including those no longer
    /// retained in `entries`.
    pub published: u64,
    /// Entries lost because a consumer fell behind the bounded channel.
    pub dropped: u64,
    /// Whether the durable file reached its byte bound and stopped growing.
    pub durable_truncated: bool,
    /// Whether the durable file could not be written, with a bounded reason.
    pub durable_error: Option<String>,
}

impl SourceHistory {
    /// The most recent entry, if any.
    pub fn latest(&self) -> Option<&SourceEventRecord> {
        self.entries.back()
    }

    /// Whether any history is known to be missing from this view.
    pub fn complete(&self) -> bool {
        self.dropped == 0 && self.published as usize <= self.entries.len()
    }
}

pub(crate) struct HistoryWriter {
    path: PathBuf,
    written: u64,
    state: SourceHistory,
    publish: watch::Sender<Arc<SourceHistory>>,
}

impl HistoryWriter {
    fn new(path: PathBuf, publish: watch::Sender<Arc<SourceHistory>>) -> Self {
        let written = std::fs::metadata(&path)
            .map(|value| value.len())
            .unwrap_or(0);
        Self {
            path,
            written,
            state: SourceHistory::default(),
            publish,
        }
    }

    fn append(&mut self, record: SourceEventRecord, dropped: u64) {
        self.state.published += 1;
        self.state.dropped = dropped;
        if self.state.entries.len() >= HISTORY_WINDOW_ENTRIES {
            self.state.entries.pop_front();
        }
        self.state.entries.push_back(record.clone());
        self.persist(&record);
        let _ = self.publish.send(Arc::new(self.state.clone()));
    }

    fn persist(&mut self, record: &SourceEventRecord) {
        if self.state.durable_truncated || self.state.durable_error.is_some() {
            return;
        }
        let mut line = match serde_json::to_vec(record) {
            Ok(line) => line,
            Err(error) => {
                self.state.durable_error = Some(bounded(error.to_string()));
                return;
            }
        };
        line.push(b'\n');
        if self.written.saturating_add(line.len() as u64) > HISTORY_FILE_BYTES {
            self.state.durable_truncated = true;
            return;
        }
        match self.write_line(&line) {
            Ok(()) => self.written += line.len() as u64,
            Err(error) => self.state.durable_error = Some(bounded(error.to_string())),
        }
    }

    fn write_line(&self, line: &[u8]) -> std::io::Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        file.write_all(line)?;
        file.flush()
    }

    fn finish(&mut self, dropped: u64) {
        if dropped != self.state.dropped {
            self.state.dropped = dropped;
            let _ = self.publish.send(Arc::new(self.state.clone()));
        }
    }
}

fn bounded(text: String) -> String {
    lvu_core::source_event::bounded_detail(text)
}

/// Reads a source's durable history file, bounded by entry count. Malformed
/// trailing content is reported rather than silently dropped.
pub fn read_history(
    path: &Path,
    maximum_entries: usize,
) -> std::io::Result<Vec<SourceEventRecord>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut entries = Vec::new();
    for line in bytes.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        if entries.len() >= maximum_entries {
            break;
        }
        entries.push(serde_json::from_slice(line).map_err(|error| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
        })?);
    }
    Ok(entries)
}

/// Drains capture history into the durable file and the published snapshot.
/// The drain owns blocking file I/O on its own thread so capture is never
/// stalled by storage.
pub(crate) fn spawn_history(
    path: PathBuf,
    mut receiver: mpsc::Receiver<SourceEventRecord>,
    dropped: Arc<AtomicU64>,
) -> (
    watch::Receiver<Arc<SourceHistory>>,
    tokio::task::JoinHandle<()>,
) {
    let (publish, subscribe) = watch::channel(Arc::new(SourceHistory::default()));
    let task = tokio::task::spawn_blocking(move || {
        let mut writer = HistoryWriter::new(path, publish);
        while let Some(record) = receiver.blocking_recv() {
            let lost = dropped.load(Ordering::Relaxed);
            writer.append(record, lost);
        }
        writer.finish(dropped.load(Ordering::Relaxed));
    });
    (subscribe, task)
}
