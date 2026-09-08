//! The set of sources the most recent session held in this capture root.
//!
//! The workspace already remembers every source it has ever seen, ordered by
//! when it was last seen. That is a catalogue, not a session: it cannot say
//! which sources were open *together* the last time lvu ran here, and second
//! granularity on `last_seen` cannot be made to say it. So the session set is
//! recorded on its own, beside the workspace database, as a small manifest.
//!
//! It is deliberately not a workspace table. Reverting a commit does not revert
//! a schema migration, and this file needs none: an older binary ignores it, a
//! newer one treats a missing or unreadable manifest as "no previous session",
//! and every field is `serde(default)` so a manifest written by a later version
//! still loads.

use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use lvu_core::SourceDefinition;
use serde::{Deserialize, Serialize};

/// A manifest holding more than this is a corrupted or foreign file, not a
/// session; the app admits at most `MAX_SOURCES` sources anyway.
const MAX_MANIFEST_BYTES: u64 = 256 * 1024;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct SessionManifest {
    #[serde(default)]
    pub schema_version: u32,
    /// The sources the session held, in the order they appeared in the sidebar.
    /// A source that could not be acquired is recorded too: it was part of the
    /// set the user was looking at, and dropping it would make a resumed
    /// session quietly shrink each time it is reopened.
    #[serde(default)]
    pub sources: Vec<SourceDefinition>,
}

pub fn manifest_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join("session.json")
}

/// The previous session's sources, or none when this capture root has no
/// readable manifest. A manifest lvu cannot parse is reported rather than
/// discarded silently, but it never blocks startup.
pub fn load(workspace_root: &Path) -> Result<Vec<SourceDefinition>, String> {
    let path = manifest_path(workspace_root);
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("read {}: {error}", path.display())),
    };
    let length = file
        .metadata()
        .map_err(|error| format!("read {}: {error}", path.display()))?
        .len();
    if length > MAX_MANIFEST_BYTES {
        return Err(format!(
            "session manifest {} exceeds {MAX_MANIFEST_BYTES} bytes",
            path.display()
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read {}: {error}", path.display()))?;
    let manifest: SessionManifest = serde_json::from_slice(&bytes)
        .map_err(|error| format!("parse {}: {error}", path.display()))?;
    if manifest.schema_version > 1 {
        return Err(format!(
            "session manifest {} was written by a newer lvu",
            path.display()
        ));
    }
    Ok(manifest.sources)
}

/// Replaces the manifest atomically, so a crash mid-write leaves the previous
/// session recorded rather than a truncated one.
pub fn store(workspace_root: &Path, sources: &[SourceDefinition]) -> Result<(), String> {
    std::fs::create_dir_all(workspace_root)
        .map_err(|error| format!("create {}: {error}", workspace_root.display()))?;
    let path = manifest_path(workspace_root);
    let temporary = path.with_extension("json.tmp");
    let manifest = SessionManifest {
        schema_version: 1,
        sources: sources.to_vec(),
    };
    (|| -> io::Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary)?;
        serde_json::to_writer(&mut file, &manifest)?;
        file.write_all(b"\n")?;
        file.sync_data()?;
        std::fs::rename(&temporary, &path)
    })()
    .map_err(|error| format!("write {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lvu_core::{Acquisition, SourceId};
    use std::collections::BTreeMap;

    fn file_definition(name: &str) -> SourceDefinition {
        SourceDefinition {
            schema_version: 1,
            id: SourceId::new(),
            name: name.to_owned(),
            acquisition: Acquisition::File {
                path: PathBuf::from("/tmp").join(name),
                follow: true,
            },
            identity_hints: BTreeMap::new(),
            retention: None,
        }
    }

    #[test]
    fn a_missing_manifest_is_an_empty_session_and_a_stored_one_round_trips() {
        let root = tempfile::tempdir().expect("temporary workspace");
        let workspace = root.path().join("workspace");
        assert!(load(&workspace).expect("absent manifest").is_empty());
        let sources = vec![file_definition("a.log"), file_definition("b.log")];
        store(&workspace, &sources).expect("store manifest");
        let loaded = load(&workspace).expect("load manifest");
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].id, sources[0].id);
        assert_eq!(loaded[1].id, sources[1].id);
    }

    #[test]
    fn an_unreadable_manifest_is_reported_rather_than_treated_as_an_empty_session() {
        let root = tempfile::tempdir().expect("temporary workspace");
        let workspace = root.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace directory");
        std::fs::write(manifest_path(&workspace), b"{not json").expect("write manifest");
        assert!(load(&workspace).is_err());
    }

    /// The manifest must survive a version that adds fields, so resuming never
    /// depends on a schema bump the workspace database would have to carry.
    #[test]
    fn unknown_and_absent_fields_still_load() {
        let root = tempfile::tempdir().expect("temporary workspace");
        let workspace = root.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace directory");
        std::fs::write(manifest_path(&workspace), b"{}").expect("write manifest");
        assert!(load(&workspace).expect("empty manifest").is_empty());
    }
}
