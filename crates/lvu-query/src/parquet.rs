//! Narrow atomic Parquet part writer used by immutable local investigations.

use polars::{
    io::parquet::write::BatchedWriter,
    prelude::{DataFrame, ParquetWriter, Schema},
};
use std::{
    fs, io,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

struct LimitedWriter {
    file: fs::File,
    file_remaining: u64,
    accounting: Arc<FileAccounting>,
}

impl Write for LimitedWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let wanted = self.file_remaining.min(buffer.len() as u64);
        let allowed = self.accounting.budget.reserve(wanted);
        if allowed == 0 {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "Parquet part byte limit reached",
            ));
        }
        match self.file.write(&buffer[..allowed as usize]) {
            Ok(written) => {
                let written = written as u64;
                self.file_remaining -= written;
                self.accounting.written.fetch_add(written, Ordering::AcqRel);
                self.accounting.budget.release(allowed - written);
                Ok(written as usize)
            }
            Err(error) => {
                self.accounting.budget.release(allowed);
                Err(error)
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

#[derive(Clone)]
pub struct ParquetWriteBudget {
    inner: Arc<SharedBudget>,
}

struct SharedBudget {
    maximum: u64,
    remaining: AtomicU64,
}

impl ParquetWriteBudget {
    pub fn new(maximum_bytes: u64) -> io::Result<Self> {
        if maximum_bytes == 0 {
            return Err(limit_error());
        }
        Ok(Self {
            inner: Arc::new(SharedBudget {
                maximum: maximum_bytes,
                remaining: AtomicU64::new(maximum_bytes),
            }),
        })
    }

    pub fn bytes_written(&self) -> u64 {
        self.inner
            .maximum
            .saturating_sub(self.inner.remaining.load(Ordering::Acquire))
    }

    fn reserve(&self, wanted: u64) -> u64 {
        let mut current = self.inner.remaining.load(Ordering::Acquire);
        loop {
            let reserved = current.min(wanted);
            if reserved == 0 {
                return 0;
            }
            match self.inner.remaining.compare_exchange_weak(
                current,
                current - reserved,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return reserved,
                Err(changed) => current = changed,
            }
        }
    }

    fn release(&self, bytes: u64) {
        if bytes > 0 {
            self.inner.remaining.fetch_add(bytes, Ordering::AcqRel);
        }
    }
}

struct FileAccounting {
    budget: ParquetWriteBudget,
    written: AtomicU64,
    released: AtomicBool,
}

impl FileAccounting {
    fn release(&self) {
        if !self.released.swap(true, Ordering::AcqRel) {
            self.budget.release(self.written.swap(0, Ordering::AcqRel));
        }
    }
}

/// Writes one already-bounded frame beside its destination and atomically
/// publishes it without replacement. The caller owns row/working-memory admission; this
/// helper enforces the supplied on-disk byte allowance while encoding.
pub fn write_parquet_part(
    path: &Path,
    frame: &mut DataFrame,
    maximum_bytes: u64,
) -> io::Result<u64> {
    if maximum_bytes == 0 {
        return Err(io::Error::new(
            io::ErrorKind::FileTooLarge,
            "Parquet part byte limit reached",
        ));
    }
    let budget = ParquetWriteBudget::new(maximum_bytes)?;
    let mut writer =
        AtomicParquetPartWriter::create(path, frame.schema().as_ref(), maximum_bytes, budget)?;
    writer.write_row_group(frame)?;
    writer.finish()
}

/// Incremental atomic Parquet writer. Every `write_row_group` input remains a
/// caller-bounded frame; no whole-part concatenation is retained in memory.
pub struct AtomicParquetPartWriter {
    destination: PathBuf,
    temporary: PathBuf,
    writer: Option<BatchedWriter<LimitedWriter>>,
    schema: Schema,
    accounting: Arc<FileAccounting>,
    finished: bool,
}

impl AtomicParquetPartWriter {
    pub fn create(
        path: &Path,
        schema: &Schema,
        maximum_bytes: u64,
        budget: ParquetWriteBudget,
    ) -> io::Result<Self> {
        if maximum_bytes == 0 {
            return Err(limit_error());
        }
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid part filename"))?;
        let temporary = path.with_file_name(format!(".{name}.partial"));
        if path.exists() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "Parquet destination already exists",
            ));
        }
        let accounting = Arc::new(FileAccounting {
            budget,
            written: AtomicU64::new(0),
            released: AtomicBool::new(false),
        });
        let file = LimitedWriter {
            file: fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?,
            file_remaining: maximum_bytes,
            accounting: Arc::clone(&accounting),
        };
        let writer = match ParquetWriter::new(file).batched(schema) {
            Ok(writer) => writer,
            Err(error) => {
                cleanup_candidate(&temporary, &accounting)?;
                return Err(io::Error::other(error));
            }
        };
        Ok(Self {
            destination: path.to_owned(),
            temporary,
            writer: Some(writer),
            schema: schema.clone(),
            accounting,
            finished: false,
        })
    }

    pub fn write_row_group(&mut self, frame: &mut DataFrame) -> io::Result<()> {
        if frame.schema().as_ref() != &self.schema {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Parquet row group schema changed",
            ));
        }
        frame.align_chunks();
        self.writer
            .as_mut()
            .ok_or_else(|| io::Error::other("Parquet part is already finished"))?
            .write_batch(frame)
            .map_err(io::Error::other)
    }

    pub fn bytes_written(&self) -> io::Result<u64> {
        fs::metadata(&self.temporary).map(|metadata| metadata.len())
    }

    pub fn finish(mut self) -> io::Result<u64> {
        let writer = self
            .writer
            .take()
            .ok_or_else(|| io::Error::other("Parquet part is already finished"))?;
        if let Err(error) = writer.finish().map_err(io::Error::other) {
            cleanup_candidate(&self.temporary, &self.accounting)?;
            return Err(error);
        }
        let bytes = match fs::metadata(&self.temporary) {
            Ok(metadata) => metadata.len(),
            Err(error) => {
                cleanup_candidate(&self.temporary, &self.accounting)?;
                return Err(error);
            }
        };
        // Linking is the create-new publication step: unlike rename on Unix,
        // it cannot replace a destination that appeared while encoding.
        if let Err(error) = fs::hard_link(&self.temporary, &self.destination) {
            cleanup_candidate(&self.temporary, &self.accounting)?;
            return Err(error);
        }
        match remove_candidate(&self.temporary) {
            Ok(()) => {
                // The destination now owns the reservation permanently.
                self.finished = true;
                Ok(bytes)
            }
            Err(error) => {
                // Publication succeeded. Keep its reservation charged even though
                // removing the second hard link failed, and report no byte result.
                self.finished = true;
                Err(error)
            }
        }
    }
}

impl Drop for AtomicParquetPartWriter {
    fn drop(&mut self) {
        if !self.finished {
            drop(self.writer.take());
            let _ = cleanup_candidate(&self.temporary, &self.accounting);
        }
    }
}

fn cleanup_candidate(path: &Path, accounting: &FileAccounting) -> io::Result<()> {
    remove_candidate(path)?;
    accounting.release();
    Ok(())
}

fn remove_candidate(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn limit_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::FileTooLarge,
        "Parquet part byte limit reached",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use polars::prelude::{NamedFrom, ParquetReader, SerReader, Series};

    #[test]
    fn byte_limit_leaves_neither_destination_nor_partial_file() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("part.parquet");
        let mut frame =
            DataFrame::new(2, vec![Series::new("value".into(), ["a", "b"]).into()]).unwrap();
        assert!(write_parquet_part(&destination, &mut frame, 1).is_err());
        assert!(!destination.exists());
        assert!(!root.path().join(".part.parquet.partial").exists());
    }

    #[test]
    fn incremental_writer_preserves_row_groups_and_cleans_failed_candidate() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("incremental.parquet");
        let mut first =
            DataFrame::new(2, vec![Series::new("value".into(), [1_i64, 2]).into()]).unwrap();
        let mut second =
            DataFrame::new(2, vec![Series::new("value".into(), [3_i64, 4]).into()]).unwrap();
        let budget = ParquetWriteBudget::new(1024 * 1024).unwrap();
        let mut writer = AtomicParquetPartWriter::create(
            &destination,
            first.schema().as_ref(),
            1024 * 1024,
            budget,
        )
        .unwrap();
        writer.write_row_group(&mut first).unwrap();
        writer.write_row_group(&mut second).unwrap();
        assert!(writer.bytes_written().unwrap() > 0);
        writer.finish().unwrap();
        let frame = ParquetReader::new(fs::File::open(&destination).unwrap())
            .finish()
            .unwrap();
        assert_eq!(
            frame
                .column("value")
                .unwrap()
                .i64()
                .unwrap()
                .into_no_null_iter()
                .collect::<Vec<_>>(),
            [1, 2, 3, 4]
        );

        let failed = root.path().join("failed.parquet");
        let budget = ParquetWriteBudget::new(1).unwrap();
        if let Ok(mut writer) =
            AtomicParquetPartWriter::create(&failed, first.schema().as_ref(), 1, budget)
        {
            assert!(writer.write_row_group(&mut first).is_err());
            drop(writer);
        }
        assert!(!failed.exists());
        assert!(!root.path().join(".failed.parquet.partial").exists());
    }

    #[test]
    fn shared_budget_never_overcommits_two_active_candidates() {
        let root = tempfile::tempdir().unwrap();
        let mut frame = DataFrame::new(
            64,
            vec![
                Series::new(
                    "value".into(),
                    (0..64)
                        .map(|index| format!("row-{index:04}-{}", "x".repeat(128)))
                        .collect::<Vec<_>>(),
                )
                .into(),
            ],
        )
        .unwrap();
        let baseline_path = root.path().join("baseline.parquet");
        let baseline_budget = ParquetWriteBudget::new(1024 * 1024).unwrap();
        let mut baseline = AtomicParquetPartWriter::create(
            &baseline_path,
            frame.schema().as_ref(),
            1024 * 1024,
            baseline_budget,
        )
        .unwrap();
        baseline.write_row_group(&mut frame).unwrap();
        let one_file = baseline.finish().unwrap();
        let maximum = one_file + one_file / 2;
        let budget = ParquetWriteBudget::new(maximum).unwrap();
        let mut first = AtomicParquetPartWriter::create(
            &root.path().join("first.parquet"),
            frame.schema().as_ref(),
            1024 * 1024,
            budget.clone(),
        )
        .unwrap();
        let mut second = AtomicParquetPartWriter::create(
            &root.path().join("second.parquet"),
            frame.schema().as_ref(),
            1024 * 1024,
            budget.clone(),
        )
        .unwrap();
        let first_write = first.write_row_group(&mut frame);
        assert!(budget.bytes_written() <= maximum);
        let second_write = second.write_row_group(&mut frame);
        assert!(budget.bytes_written() <= maximum);
        let first_result = first_write.and_then(|()| first.finish());
        assert!(budget.bytes_written() <= maximum);
        let second_result = second_write.and_then(|()| second.finish());
        assert!(budget.bytes_written() <= maximum);
        assert!(first_result.is_err() || second_result.is_err());
        let actual = [
            "first.parquet",
            "second.parquet",
            ".first.parquet.partial",
            ".second.parquet.partial",
        ]
        .iter()
        .map(|name| fs::metadata(root.path().join(name)).map_or(0, |value| value.len()))
        .sum::<u64>();
        assert!(actual <= maximum);
        assert_eq!(budget.bytes_written(), actual);
    }

    #[test]
    fn create_new_preserves_existing_destination_and_partial() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("part.parquet");
        let partial = root.path().join(".part.parquet.partial");
        fs::write(&destination, b"destination").unwrap();
        fs::write(&partial, b"partial").unwrap();
        let frame = DataFrame::new(1, vec![Series::new("value".into(), [1_i64]).into()]).unwrap();
        let budget = ParquetWriteBudget::new(1024).unwrap();
        assert!(
            AtomicParquetPartWriter::create(&destination, frame.schema().as_ref(), 1024, budget,)
                .is_err()
        );
        assert_eq!(fs::read(&destination).unwrap(), b"destination");
        assert_eq!(fs::read(&partial).unwrap(), b"partial");
    }

    #[test]
    fn failed_candidate_unlink_keeps_its_budget_reserved() {
        let root = tempfile::tempdir().unwrap();
        let candidate = root.path().join("candidate");
        fs::create_dir(&candidate).unwrap();
        let budget = ParquetWriteBudget::new(32).unwrap();
        assert_eq!(budget.reserve(7), 7);
        let accounting = FileAccounting {
            budget: budget.clone(),
            written: AtomicU64::new(7),
            released: AtomicBool::new(false),
        };
        assert!(cleanup_candidate(&candidate, &accounting).is_err());
        assert_eq!(budget.bytes_written(), 7);
        fs::remove_dir(&candidate).unwrap();
        cleanup_candidate(&candidate, &accounting).unwrap();
        assert_eq!(budget.bytes_written(), 0);
    }
}
