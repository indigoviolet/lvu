use lvu_core::{SourceDefinition, SourceId};
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::Path,
};
use uuid::Uuid;

#[derive(Debug, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub(crate) enum CatalogEvent<'a> {
    Starting {
        generation: u64,
    },
    Running,
    StartFailed {
        message: &'a str,
    },
    Boundary {
        acquisition_id: Uuid,
        reason: &'a str,
    },
    CommandExit {
        acquisition_id: Uuid,
        code: Option<i32>,
        success: bool,
    },
    Error {
        acquisition_id: Uuid,
        message: &'a str,
    },
    StorageBlocked {
        limit_bytes: u64,
        discarded_buffered_bytes: u64,
        discarded_bytes_known: bool,
    },
    Stopped,
    Aborted {
        discarded_buffered_bytes: u64,
        discarded_bytes_known: bool,
    },
    Incomplete {
        discarded_buffered_bytes: u64,
        discarded_bytes_known: bool,
        reason: &'a str,
    },
}

pub(crate) struct Catalog {
    file: File,
}

impl Catalog {
    pub(crate) fn open(path: &Path) -> io::Result<Self> {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)?;
        let recovered = recover_catalog(&mut file)?;
        file.seek(SeekFrom::End(0))?;
        let mut catalog = Self { file };
        if recovered {
            catalog.record(CatalogEvent::Incomplete {
                discarded_buffered_bytes: 0,
                discarded_bytes_known: false,
                reason: "recovered torn catalog tail from an incomplete previous run",
            })?;
        }
        Ok(catalog)
    }

    pub(crate) fn record(&mut self, event: CatalogEvent<'_>) -> io::Result<()> {
        serde_json::to_writer(&mut self.file, &event)?;
        self.file.write_all(b"\n")?;
        self.file.flush()?;
        self.file.sync_data()
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct SourceMetadata {
    pub schema_version: u32,
    pub source_id: SourceId,
    pub generation: u64,
    pub definition: SourceDefinition,
}

pub(crate) fn write_metadata(path: &Path, metadata: &SourceMetadata) -> io::Result<()> {
    let temporary = path.with_extension("json.tmp");
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporary)?;
    serde_json::to_writer_pretty(&mut file, metadata)?;
    file.write_all(b"\n")?;
    file.sync_data()?;
    std::fs::rename(temporary, path)?;
    File::open(path.parent().unwrap_or(Path::new(".")))?.sync_all()
}

pub(crate) fn next_generation(
    path: &Path,
    source_id: SourceId,
    definition: &SourceDefinition,
) -> io::Result<u64> {
    const MAX_METADATA_BYTES: u64 = 1024 * 1024;
    match File::open(path) {
        Ok(file) => {
            if file.metadata()?.len() > MAX_METADATA_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "source metadata exceeds the bounded read limit",
                ));
            }
            let mut bytes = Vec::new();
            file.take(MAX_METADATA_BYTES + 1).read_to_end(&mut bytes)?;
            let metadata: SourceMetadata = serde_json::from_slice(&bytes)?;
            if metadata.schema_version != 1 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "unsupported source metadata schema {}",
                        metadata.schema_version
                    ),
                ));
            }
            if metadata.source_id != source_id {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "source metadata identity mismatch",
                ));
            }
            if metadata.definition != *definition {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "persisted source definition does not match the submitted definition",
                ));
            }
            metadata.generation.checked_add(1).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "source generation exhausted")
            })
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(1),
        Err(error) => Err(error),
    }
}

fn recover_catalog(file: &mut File) -> io::Result<bool> {
    const MAX_EVENT_BYTES: usize = 1024 * 1024;
    file.seek(SeekFrom::Start(0))?;
    let mut buffer = [0_u8; 8192];
    let mut line = Vec::new();
    let mut valid_end = 0_u64;
    let mut position = 0_u64;
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        for byte in &buffer[..count] {
            position += 1;
            line.push(*byte);
            if line.len() > MAX_EVENT_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "catalog event exceeds the bounded recovery limit",
                ));
            }
            if *byte == b'\n' {
                serde_json::from_slice::<serde_json::Value>(&line[..line.len() - 1])
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
                line.clear();
                valid_end = position;
            }
        }
    }
    if line.is_empty() {
        return Ok(false);
    }
    file.set_len(valid_end)?;
    file.sync_data()?;
    Ok(true)
}
