use lvu_core::{FileResumeCursor, SourceId};
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};
use uuid::Uuid;

const MAX_CURSOR_BYTES: u64 = 64 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct DurableFileCursor {
    pub schema_version: u32,
    pub source_id: SourceId,
    pub path: PathBuf,
    pub acquisition_id: Uuid,
    pub journal_offset: u64,
    pub file: FileResumeCursor,
}

pub(crate) fn load(
    path: &Path,
    source_id: SourceId,
    source_path: &Path,
) -> io::Result<Option<DurableFileCursor>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if file.metadata()?.len() > MAX_CURSOR_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "file cursor exceeds bounded read limit",
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_CURSOR_BYTES + 1).read_to_end(&mut bytes)?;
    let cursor: DurableFileCursor = serde_json::from_slice(&bytes)?;
    if cursor.schema_version != 1 || cursor.source_id != source_id || cursor.path != source_path {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "file cursor schema or source identity mismatch",
        ));
    }
    Ok(Some(cursor))
}

pub(crate) fn store(path: &Path, cursor: &DurableFileCursor) -> io::Result<()> {
    let temporary = path.with_extension("json.tmp");
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporary)?;
    serde_json::to_writer(&mut file, cursor)?;
    file.write_all(b"\n")?;
    file.sync_data()?;
    std::fs::rename(&temporary, path)?;
    File::open(path.parent().unwrap_or(Path::new(".")))?.sync_all()
}
