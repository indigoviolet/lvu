//! Narrow atomic Parquet part writer used by immutable local investigations.

use polars::prelude::{DataFrame, ParquetWriter};
use std::{fs, io, io::Write, path::Path};

struct LimitedWriter {
    file: fs::File,
    remaining: u64,
}

impl Write for LimitedWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let allowed = self.remaining.min(buffer.len() as u64) as usize;
        if allowed == 0 {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "Parquet part byte limit reached",
            ));
        }
        let written = self.file.write(&buffer[..allowed])?;
        self.remaining -= written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

/// Writes one already-bounded frame beside its destination and atomically
/// renames it into place. The caller owns row/working-memory admission; this
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
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid part filename"))?;
    let temporary = path.with_file_name(format!(".{name}.partial"));
    let result = (|| {
        let file = LimitedWriter {
            file: fs::File::create(&temporary)?,
            remaining: maximum_bytes,
        };
        ParquetWriter::new(file)
            .finish(frame)
            .map_err(io::Error::other)?;
        let bytes = fs::metadata(&temporary)?.len();
        fs::rename(&temporary, path)?;
        Ok(bytes)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use polars::prelude::{NamedFrom, Series};

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
}
