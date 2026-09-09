use crate::recipe::{
    MAX_SHARED_REVISION_BYTES, MAX_SHARED_REVISIONS_PER_RECIPE, SHARED_REVISIONS_DIR,
    shared_revisions_root,
};
use crate::recipe::{RecipeLock, TimePolicy, save_recipe_locked};
use crate::{
    MAX_SEARCH_BYTES, RecipeError, RecipeFile, SavedRecipe, content_hash, read_recipe,
    validate_source,
};
use fs2::FileExt;
use lvu_core::{RecipeId, RecordId, SourceDefinition, SourceId, ViewId};
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior,
    backup::{Backup, StepResult},
    limits::Limit,
    params,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    time::Duration,
};
use uuid::Uuid;

const DB_SCHEMA_VERSION: i64 = 6;
/// Bookmarks retained per source. Matches the former per-view limit.
pub const MAX_SOURCE_BOOKMARKS: usize = 128;
/// Longest note kept after two views' notes for one record are joined.
pub const MAX_BOOKMARK_NOTE_BYTES: usize = 1024;
/// Longest accepted canonical view name.
const MAX_VIEW_NAME_BYTES: usize = 128;
/// Bounded search for a free deterministic canonical view identity.
const MAX_CANONICAL_ID_ATTEMPTS: usize = 8;
const MAX_PAGE: u32 = 100;
const MAX_RECONCILE_FILES: usize = 1024;
const MAX_SQLITE_VALUE_BYTES: i32 = 1_200_000;
const MAX_CANDIDATE_SCAN: i64 = 128;
const MAX_EDITOR_BYTES: usize = 256 * 1024;
/// Expanded repeated runs remembered per view.
const MAX_FOLD_EXPANDED: usize = 256;
/// Widest fold lookback a stored view may name. Mirrors `lvu::MAX_FOLD_LOOKBACK`;
/// the store refuses rather than silently clamping, so a corrupted value is
/// visible instead of quietly becoming a different policy.
const MAX_FOLD_LOOKBACK: u32 = 4_096;
const MAX_DIAGNOSTICS: usize = 128;
pub const MAX_COMMAND_ATTEMPT_BATCH: usize = 1024;
pub const MAX_COMMAND_ATTEMPT_FIELDS: usize = 128;
pub const MAX_COMMAND_ATTEMPT_FIELD_BYTES: usize = 128;
pub const MAX_COMMAND_ATTEMPT_RESULT_BYTES: usize = 256 * 1024;
pub const MAX_COMMAND_ATTEMPT_DIAGNOSTIC_BYTES: usize = 16 * 1024;
pub const MAX_COMMAND_ATTEMPT_BATCH_BYTES: usize = 1024 * 1024;
const MAX_COMMAND_SCOPE_COMPONENT_BYTES: usize = 128;
const MAX_LEGACY_DATABASE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_BACKUP_DURATION: Duration = Duration::from_secs(10);
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LegacyWorkspaceSeed {
    pub legacy_workspace_root: PathBuf,
    pub target_workspace_root: PathBuf,
    pub migration_root: PathBuf,
    pub completion_marker: PathBuf,
    pub version: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LegacyImportOutcome {
    Imported,
    AlreadyImported,
    NoLegacyDatabase,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct WorkspaceImportProvenance {
    version: u32,
    source_root: String,
    target_root: String,
    source_schema_version: i64,
    initial_snapshot_digest: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct FrozenWorkspaceProvenance {
    version: u32,
    path: String,
    device: u64,
    inode: u64,
    bytes: u64,
    sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecipeSeed {
    pub legacy_recipes_root: PathBuf,
    pub target_recipes_root: PathBuf,
    pub migration_root: PathBuf,
    pub version: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RecipeSeedDiagnostic {
    pub file_name: String,
    pub diagnostic: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecipeBootstrapReport {
    pub imported: usize,
    pub unavailable: Vec<RecipeSeedDiagnostic>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct RecipeBootstrapMarker {
    version: u32,
    legacy_recipes_root: String,
    target_recipes_root: String,
    imported: usize,
    unavailable: Vec<RecipeSeedDiagnostic>,
    files: BTreeMap<String, String>,
    /// Shared immutable revision documents, keyed by their namespace-relative
    /// name. Counted separately: `imported` remains the number of recipes.
    #[serde(default)]
    revisions: BTreeMap<String, String>,
}

fn validate_workspace_seed(seed: &LegacyWorkspaceSeed) -> Result<(), MemoryError> {
    if seed.version == 0
        || !seed.legacy_workspace_root.is_absolute()
        || !seed.target_workspace_root.is_absolute()
        || !seed.migration_root.is_absolute()
        || seed.completion_marker.parent() != Some(seed.migration_root.as_path())
        || seed.legacy_workspace_root == seed.target_workspace_root
    {
        return Err(MemoryError::InvalidData(
            "invalid versioned workspace seed paths".into(),
        ));
    }
    Ok(())
}

/// One admission file serialises the shared recipe namespace bootstrap and every
/// slot's private workspace import for a storage version.
fn bootstrap_admission_path(migration_root: &Path, version: u32) -> PathBuf {
    migration_root.join(format!("bootstrap-v{version}.lock"))
}

fn open_admission_lock(path: &Path) -> Result<fs::File, MemoryError> {
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|source| MemoryError::Io {
            path: path.to_path_buf(),
            source,
        })
}

fn lock_bounded(file: &fs::File, label: &str) -> Result<(), MemoryError> {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(()),
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => {
                return Err(MemoryError::InvalidData(format!(
                    "{label} admission is busy: {error}"
                )));
            }
        }
    }
}

fn migrate_connection(conn: &Connection) -> Result<(), MemoryError> {
    let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version > DB_SCHEMA_VERSION {
        return Err(MemoryError::FutureDatabase(version));
    }
    if version == 0 {
        migrate_v1(conn)?;
    }
    if version < 2 {
        migrate_v2(conn)?;
    }
    if version < 3 {
        conn.pragma_update(None, "user_version", 3)?;
    }
    if version < 4 {
        migrate_v4(conn)?;
    }
    if version < 5 {
        migrate_v5(conn)?;
    }
    if version < 6 {
        migrate_v6(conn)?;
    }
    Ok(())
}

fn hash_file_bounded(path: &Path, maximum: u64) -> Result<String, MemoryError> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file = fs::File::open(path).map_err(|source| MemoryError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if file
        .metadata()
        .map_err(|source| MemoryError::Io {
            path: path.to_path_buf(),
            source,
        })?
        .len()
        > maximum
    {
        return Err(MemoryError::InvalidData(
            "workspace snapshot exceeds its size limit".into(),
        ));
    }
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|source| MemoryError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(hex::encode(digest.finalize()))
}

fn imported_workspace_provenance(
    seed: &LegacyWorkspaceSeed,
    target_db: &Path,
) -> Result<WorkspaceImportProvenance, MemoryError> {
    let conn = Connection::open_with_flags(target_db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let value = conn.query_row(
        "SELECT version,source_root,target_root,source_schema_version,initial_snapshot_digest \
         FROM workspace_import_provenance",
        [],
        |row| {
            Ok(WorkspaceImportProvenance {
                version: row.get(0)?,
                source_root: row.get(1)?,
                target_root: row.get(2)?,
                source_schema_version: row.get(3)?,
                initial_snapshot_digest: row.get(4)?,
            })
        },
    )?;
    if value.version != seed.version
        || value.source_root != stable_path(&seed.legacy_workspace_root)?
        || value.target_root != stable_path(&seed.target_workspace_root)?
        || value.source_schema_version > DB_SCHEMA_VERSION
    {
        return Err(MemoryError::InvalidData(
            "workspace import provenance does not match this seed".into(),
        ));
    }
    Ok(value)
}

fn validate_imported_workspace(
    seed: &LegacyWorkspaceSeed,
    target_db: &Path,
) -> Result<(), MemoryError> {
    let provenance = imported_workspace_provenance(seed, target_db)?;
    if seed.completion_marker.exists() {
        let marker: WorkspaceImportProvenance =
            serde_json::from_slice(&fs::read(&seed.completion_marker).map_err(|source| {
                MemoryError::Io {
                    path: seed.completion_marker.clone(),
                    source,
                }
            })?)
            .map_err(|error| MemoryError::InvalidData(error.to_string()))?;
        if marker != provenance {
            return Err(MemoryError::InvalidData(
                "workspace marker disagrees with embedded import provenance".into(),
            ));
        }
    }
    Ok(())
}

fn publish_workspace_marker(
    seed: &LegacyWorkspaceSeed,
    target_db: &Path,
) -> Result<(), MemoryError> {
    let provenance = imported_workspace_provenance(seed, target_db)?;
    let bytes = serde_json::to_vec(&provenance)
        .map_err(|error| MemoryError::InvalidData(error.to_string()))?;
    write_new_synced(&seed.completion_marker, &bytes)?;
    sync_directory(&seed.migration_root)
}

fn initialize_empty_workspace(
    seed: &LegacyWorkspaceSeed,
    target_db: &Path,
) -> Result<(), MemoryError> {
    let pending = seed
        .migration_root
        .join(format!("workspace-v{}.empty.pending.sqlite3", seed.version));
    match fs::remove_file(&pending) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(MemoryError::Io {
                path: pending,
                source,
            });
        }
    }
    let destination = Connection::open(&pending)?;
    migrate_connection(&destination)?;
    destination.execute_batch(
        "CREATE TABLE workspace_import_provenance(\
            version INTEGER PRIMARY KEY,source_root TEXT NOT NULL,target_root TEXT NOT NULL,\
            source_schema_version INTEGER NOT NULL,initial_snapshot_digest TEXT NOT NULL);",
    )?;
    destination.execute(
        "INSERT INTO workspace_import_provenance VALUES(?1,?2,?3,0,'no-legacy-at-bootstrap')",
        params![
            seed.version,
            stable_path(&seed.legacy_workspace_root)?,
            stable_path(&seed.target_workspace_root)?
        ],
    )?;
    destination.close().map_err(|(_, error)| error)?;
    fs::File::open(&pending)
        .and_then(|file| file.sync_all())
        .map_err(|source| MemoryError::Io {
            path: pending.clone(),
            source,
        })?;
    fs::hard_link(&pending, target_db).map_err(|source| MemoryError::Io {
        path: target_db.to_path_buf(),
        source,
    })?;
    sync_directory(&seed.target_workspace_root)?;
    publish_workspace_marker(seed, target_db)?;
    fs::remove_file(&pending).map_err(|source| MemoryError::Io {
        path: pending,
        source,
    })
}

fn validate_recipe_seed(seed: &RecipeSeed) -> Result<(), MemoryError> {
    if seed.version == 0
        || !seed.legacy_recipes_root.is_absolute()
        || !seed.target_recipes_root.is_absolute()
        || !seed.migration_root.is_absolute()
        || seed.legacy_recipes_root == seed.target_recipes_root
    {
        return Err(MemoryError::InvalidData(
            "invalid versioned recipe bootstrap paths".into(),
        ));
    }
    Ok(())
}

fn stable_path(path: &Path) -> Result<String, MemoryError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| MemoryError::InvalidData("recipe bootstrap path is not UTF-8".into()))
}

fn decode_recipe_bootstrap(
    seed: &RecipeSeed,
    bytes: &[u8],
) -> Result<RecipeBootstrapReport, MemoryError> {
    let marker: RecipeBootstrapMarker = serde_json::from_slice(bytes).map_err(|error| {
        MemoryError::InvalidData(format!("invalid recipe bootstrap marker: {error}"))
    })?;
    if marker.version != seed.version
        || marker.legacy_recipes_root != stable_path(&seed.legacy_recipes_root)?
        || marker.target_recipes_root != stable_path(&seed.target_recipes_root)?
        || marker.imported > MAX_RECONCILE_FILES
        || marker.unavailable.len() > MAX_RECONCILE_FILES
        || marker.imported + marker.unavailable.len() > MAX_RECONCILE_FILES
        || marker.files.len() != marker.imported
        || marker.revisions.len() > MAX_RECONCILE_FILES * MAX_SHARED_REVISIONS_PER_RECIPE
    {
        return Err(MemoryError::InvalidData(
            "recipe bootstrap provenance does not match this namespace".into(),
        ));
    }
    Ok(RecipeBootstrapReport {
        imported: marker.imported,
        unavailable: marker.unavailable,
    })
}

fn read_embedded_recipe_bootstrap(seed: &RecipeSeed) -> Result<RecipeBootstrapReport, MemoryError> {
    let path = seed.target_recipes_root.join(".bootstrap.json");
    let bytes = fs::read(&path).map_err(|source| MemoryError::Io { path, source })?;
    let report = decode_recipe_bootstrap(seed, &bytes)?;
    Ok(report)
}

fn validate_staged_recipe_manifest(
    directory: &Path,
    marker: &RecipeBootstrapMarker,
) -> Result<(), MemoryError> {
    for (name, expected_hash) in marker.files.iter().chain(marker.revisions.iter()) {
        let path = directory.join(name);
        if hash_file_bounded(&path, crate::MAX_DEFINITION_BYTES)? != *expected_hash {
            return Err(MemoryError::InvalidData(
                "staged recipe namespace does not match its manifest".into(),
            ));
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn rename_directory_noreplace(source: &Path, target: &Path) -> Result<(), MemoryError> {
    use std::os::unix::ffi::OsStrExt;
    let source_name = std::ffi::CString::new(source.as_os_str().as_bytes())
        .map_err(|_| MemoryError::InvalidData("recipe staging path contains NUL".into()))?;
    let target_name = std::ffi::CString::new(target.as_os_str().as_bytes())
        .map_err(|_| MemoryError::InvalidData("recipe target path contains NUL".into()))?;
    // The kernel has had renameat2 since Linux 3.15, but musl exported no
    // wrapper for it until 1.2.5 and the musl that Rust bundles for
    // x86_64-unknown-linux-musl is older, so a declared `renameat2` symbol does
    // not link in the statically linked Linux release build. The syscall is
    // issued directly instead, and the flag and directory constants are now
    // named rather than written as the bare -100 and 1 they used to be.
    //
    // Every integer argument is widened to c_long first. syscall(2) is variadic
    // and reads each argument with va_arg(long); default argument promotion
    // stops at int, so a 32-bit AT_FDCWD would leave the upper half of the
    // register unspecified and could reach the kernel as 4294967196.
    //
    // SAFETY: both paths are valid NUL-terminated strings; renameat2 does not
    // retain the pointers and RENAME_NOREPLACE preserves an existing winner.
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD as libc::c_long,
            source_name.as_ptr(),
            libc::AT_FDCWD as libc::c_long,
            target_name.as_ptr(),
            libc::RENAME_NOREPLACE as libc::c_long,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(MemoryError::Io {
            path: target.to_path_buf(),
            source: std::io::Error::last_os_error(),
        })
    }
}

#[cfg(not(target_os = "linux"))]
fn rename_directory_noreplace(source: &Path, target: &Path) -> Result<(), MemoryError> {
    if target.exists() {
        return Err(MemoryError::InvalidData(
            "recipe namespace already exists".into(),
        ));
    }
    fs::rename(source, target).map_err(|source_error| MemoryError::Io {
        path: target.to_path_buf(),
        source: source_error,
    })
}

fn write_new_synced(path: &Path, bytes: &[u8]) -> Result<(), MemoryError> {
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|source| MemoryError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    file.write_all(bytes).map_err(|source| MemoryError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    file.sync_all().map_err(|source| MemoryError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn sync_directory(path: &Path) -> Result<(), MemoryError> {
    fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| MemoryError::Io {
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(unix)]
fn file_identity(metadata: &fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (metadata.dev(), metadata.ino())
}

#[cfg(not(unix))]
fn file_identity(_metadata: &fs::Metadata) -> (u64, u64) {
    (0, 0)
}

fn frozen_seed_paths(seed: &RecipeSeed) -> (PathBuf, PathBuf) {
    (
        seed.migration_root
            .join(format!("workspace-v{}.seed.sqlite3", seed.version)),
        seed.migration_root
            .join(format!("workspace-v{}.seed.json", seed.version)),
    )
}

/// Opens the frozen seed without following a symlink, so the returned handle is
/// the exact regular file bound by immutable provenance.
#[cfg(target_os = "linux")]
fn open_frozen_seed_handle(path: &Path) -> Result<fs::File, MemoryError> {
    use std::os::unix::fs::OpenOptionsExt;
    // Linux O_NOFOLLOW | O_CLOEXEC.
    const O_NOFOLLOW: i32 = 0x2_0000;
    const O_CLOEXEC: i32 = 0x8_0000;
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(O_NOFOLLOW | O_CLOEXEC)
        .open(path)
        .map_err(|source| MemoryError::Io {
            path: path.to_path_buf(),
            source,
        })
}

/// Portable fallback: refuse a symlinked seed before opening it. This check is
/// not atomic with the open, so the provenance identity comparison below stays
/// the authority on which file was actually read.
#[cfg(not(target_os = "linux"))]
fn open_frozen_seed_handle(path: &Path) -> Result<fs::File, MemoryError> {
    let link = fs::symlink_metadata(path).map_err(|source| MemoryError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if link.file_type().is_symlink() || !link.is_file() {
        return Err(MemoryError::InvalidData(
            "frozen workspace seed is not a regular file".into(),
        ));
    }
    fs::File::open(path).map_err(|source| MemoryError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn open_validated_frozen_seed(
    seed: &RecipeSeed,
) -> Result<(fs::File, FrozenWorkspaceProvenance), MemoryError> {
    let (frozen, provenance_path) = frozen_seed_paths(seed);
    let provenance: FrozenWorkspaceProvenance = serde_json::from_slice(
        &fs::read(&provenance_path).map_err(|source| MemoryError::Io {
            path: provenance_path.clone(),
            source,
        })?,
    )
    .map_err(|error| MemoryError::InvalidData(error.to_string()))?;
    let file = open_frozen_seed_handle(&frozen)?;
    let metadata = file.metadata().map_err(|source| MemoryError::Io {
        path: frozen.clone(),
        source,
    })?;
    let (device, inode) = file_identity(&metadata);
    if !metadata.is_file()
        || provenance.version != seed.version
        || provenance.path != stable_path(&frozen)?
        || provenance.device != device
        || provenance.inode != inode
        || provenance.bytes != metadata.len()
        || provenance.sha256 != hash_file_handle_bounded(&file, MAX_LEGACY_DATABASE_BYTES, &frozen)?
    {
        return Err(MemoryError::InvalidData(
            "frozen workspace seed does not match its immutable provenance".into(),
        ));
    }
    Ok((file, provenance))
}

/// Hashes the whole file behind a handle without consuming its shared offset.
/// `try_clone` duplicates the file description, so both the validation hash and
/// the post-backup verification hash must reposition explicitly; otherwise the
/// second read starts at end-of-file and digests an empty suffix.
fn hash_file_handle_bounded(
    file: &fs::File,
    maximum: u64,
    path: &Path,
) -> Result<String, MemoryError> {
    use sha2::{Digest, Sha256};
    use std::io::{Read, Seek, SeekFrom};
    let mut reader = file.try_clone().map_err(|source| MemoryError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    reader
        .seek(SeekFrom::Start(0))
        .map_err(|source| MemoryError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    let mut digest = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer).map_err(|source| MemoryError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        if read == 0 {
            break;
        }
        total += read as u64;
        if total > maximum {
            return Err(MemoryError::InvalidData(
                "workspace seed exceeds its size limit".into(),
            ));
        }
        digest.update(&buffer[..read]);
    }
    Ok(hex::encode(digest.finalize()))
}

fn freeze_legacy_workspace(seed: &RecipeSeed) -> Result<(), MemoryError> {
    let source_path = seed
        .legacy_recipes_root
        .parent()
        .ok_or_else(|| MemoryError::InvalidData("legacy recipe root has no workspace".into()))?
        .join("workspace.sqlite3");
    let (frozen, provenance_path) = frozen_seed_paths(seed);
    // Publication of the pair is not atomic, so both interrupted orderings must
    // be repaired here rather than left to wedge a later retry.
    match (frozen.exists(), provenance_path.exists()) {
        (true, true) => {
            open_validated_frozen_seed(seed)?;
            return Ok(());
        }
        (true, false) => {
            // A seed nothing can vouch for. Discard it and freeze again.
            fs::remove_file(&frozen).map_err(|source| MemoryError::Io {
                path: frozen.clone(),
                source,
            })?;
        }
        (false, true) => {
            // Provenance names a device/inode that no longer exists, so it can
            // never be satisfied again. Without a legacy source to re-freeze
            // from, fail closed: importing an empty workspace here would
            // silently discard the seed this namespace was promised.
            if !source_path.exists() {
                return Err(MemoryError::InvalidData(
                    "frozen workspace seed provenance has no seed and no legacy source".into(),
                ));
            }
            fs::remove_file(&provenance_path).map_err(|source| MemoryError::Io {
                path: provenance_path.clone(),
                source,
            })?;
        }
        (false, false) => {}
    }
    if !source_path.exists() {
        return Ok(());
    }
    let pending = seed
        .migration_root
        .join(format!("workspace-v{}.seed.pending", seed.version));
    let _ = fs::remove_file(&pending);
    let source = Connection::open_with_flags(
        &source_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let mut destination = Connection::open(&pending)?;
    let started = std::time::Instant::now();
    {
        let backup = Backup::new(&source, &mut destination)?;
        loop {
            match backup.step(128)? {
                StepResult::Done => break,
                StepResult::More
                    if started.elapsed() <= MAX_BACKUP_DURATION
                        && backup.progress().pagecount <= 65_536 => {}
                StepResult::Busy | StepResult::Locked
                    if started.elapsed() <= MAX_BACKUP_DURATION =>
                {
                    std::thread::sleep(Duration::from_millis(5));
                }
                _ => {
                    return Err(MemoryError::InvalidData(
                        "legacy workspace seed exceeded its bounded admission".into(),
                    ));
                }
            }
        }
    }
    destination.close().map_err(|(_, error)| error)?;
    fs::File::open(&pending)
        .and_then(|file| file.sync_all())
        .map_err(|source| MemoryError::Io {
            path: pending.clone(),
            source,
        })?;
    fs::hard_link(&pending, &frozen).map_err(|source| MemoryError::Io {
        path: frozen.clone(),
        source,
    })?;
    let metadata = fs::metadata(&frozen).map_err(|source| MemoryError::Io {
        path: frozen.clone(),
        source,
    })?;
    let (device, inode) = file_identity(&metadata);
    let provenance = FrozenWorkspaceProvenance {
        version: seed.version,
        path: stable_path(&frozen)?,
        device,
        inode,
        bytes: metadata.len(),
        sha256: hash_file_bounded(&frozen, MAX_LEGACY_DATABASE_BYTES)?,
    };
    write_new_synced(
        &provenance_path,
        &serde_json::to_vec(&provenance)
            .map_err(|error| MemoryError::InvalidData(error.to_string()))?,
    )?;
    sync_directory(&seed.migration_root)?;
    fs::remove_file(&pending).map_err(|source| MemoryError::Io {
        path: pending,
        source,
    })
}

/// Opens the validated frozen seed through its held descriptor. The handle must
/// outlive the connection so the bound inode cannot be replaced underneath it.
fn open_frozen_seed_connection(seed: &RecipeSeed) -> Result<(Connection, fs::File), MemoryError> {
    let (frozen, _) = frozen_seed_paths(seed);
    let (handle, _provenance) = open_validated_frozen_seed(seed)?;
    #[cfg(target_os = "linux")]
    let open_path = {
        use std::os::fd::AsRawFd;
        PathBuf::from(format!("/proc/self/fd/{}", handle.as_raw_fd()))
    };
    #[cfg(not(target_os = "linux"))]
    let open_path = frozen.clone();
    let _ = &frozen;
    let conn = Connection::open_with_flags(
        &open_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(Duration::from_secs(2))?;
    Ok((conn, handle))
}

fn shared_revision_name(recipe_id: &str, revision_id: &str) -> String {
    format!("{SHARED_REVISIONS_DIR}/{recipe_id}/{revision_id}.toml")
}

/// Stages every bounded legacy revision, not only the current pointer, so a slot
/// that bootstraps after another slot has already published still reconciles the
/// complete history the legacy workspace held.
fn stage_legacy_revisions(
    seed: &RecipeSeed,
    pending: &Path,
    revisions: &mut BTreeMap<String, String>,
    unavailable: &mut Vec<RecipeSeedDiagnostic>,
) -> Result<(), MemoryError> {
    let (frozen, provenance_path) = frozen_seed_paths(seed);
    if !frozen.exists() || !provenance_path.exists() {
        return Ok(());
    }
    let (conn, handle) = open_frozen_seed_connection(seed)?;
    let present: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='recipe_revisions')",
        [],
        |row| row.get(0),
    )?;
    if !present {
        return Ok(());
    }
    let mut per_recipe: BTreeMap<String, usize> = BTreeMap::new();
    let mut truncated: BTreeSet<String> = BTreeSet::new();
    let mut total_bytes = 0u64;
    {
        let mut statement = conn.prepare(
            "SELECT recipe_id,revision_id,document FROM recipe_revisions ORDER BY recipe_id,rowid",
        )?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let recipe_id: String = row.get(0)?;
            let revision_id: String = row.get(1)?;
            let document: Vec<u8> = row.get(2)?;
            let name = shared_revision_name(&recipe_id, &revision_id);
            let staged = per_recipe.entry(recipe_id.clone()).or_default();
            if *staged >= MAX_SHARED_REVISIONS_PER_RECIPE {
                truncated.insert(recipe_id);
                continue;
            }
            total_bytes += document.len() as u64;
            if total_bytes > MAX_SHARED_REVISION_BYTES {
                return Err(MemoryError::ReconcileLimit);
            }
            let parsed = std::str::from_utf8(&document)
                .map_err(|error| error.to_string())
                .and_then(|text| {
                    toml::from_str::<RecipeFile>(text).map_err(|error| error.to_string())
                })
                .and_then(|value| value.validate().map(|()| value).map_err(|e| e.to_string()));
            match parsed {
                Ok(value)
                    if value.recipe_id.0.to_string() == recipe_id
                        && value.revision_id.to_string() == revision_id =>
                {
                    let directory = pending.join(SHARED_REVISIONS_DIR).join(&recipe_id);
                    fs::create_dir_all(&directory).map_err(|source| MemoryError::Io {
                        path: directory.clone(),
                        source,
                    })?;
                    write_new_synced(&directory.join(format!("{revision_id}.toml")), &document)?;
                    sync_directory(&directory)?;
                    revisions.insert(name, content_hash(&document));
                    *staged += 1;
                }
                Ok(_) => unavailable.push(RecipeSeedDiagnostic {
                    file_name: name,
                    diagnostic: "legacy revision document does not match its identity".into(),
                }),
                Err(diagnostic) => unavailable.push(RecipeSeedDiagnostic {
                    file_name: name,
                    diagnostic,
                }),
            }
        }
    }
    drop(conn);
    drop(handle);
    for recipe_id in truncated {
        unavailable.push(RecipeSeedDiagnostic {
            file_name: format!("{SHARED_REVISIONS_DIR}/{recipe_id}"),
            diagnostic: format!(
                "legacy revision history truncated at {MAX_SHARED_REVISIONS_PER_RECIPE} revisions"
            ),
        });
    }
    if !revisions.is_empty() {
        sync_directory(&pending.join(SHARED_REVISIONS_DIR))?;
    }
    Ok(())
}

fn source_family(definition: &SourceDefinition) -> &'static str {
    match definition.acquisition {
        lvu_core::Acquisition::File { .. } => "file",
        lvu_core::Acquisition::Command { .. } => "command",
        lvu_core::Acquisition::Http { .. } => "http",
        lvu_core::Acquisition::Stdin => "stdin",
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error(transparent)]
    Recipe(#[from] RecipeError),
    #[error("workspace database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("stored data is invalid: {0}")]
    InvalidData(String),
    #[error("unsupported database schema version {0}")]
    FutureDatabase(i64),
    #[error("concurrent update conflict")]
    Conflict,
    #[error("page limit must be between 1 and {MAX_PAGE}")]
    InvalidLimit,
    #[error("invalid command attempt batch: {0}")]
    InvalidAttemptBatch(String),
    #[error("record {0:?} was already attempted for this command definition")]
    AlreadyAttempted(RecordId),
    #[error(
        "command attempt capacity exceeded: {existing} stored + {requested} requested > {capacity}"
    )]
    AttemptCapacity {
        existing: usize,
        requested: usize,
        capacity: usize,
    },
    #[error("command attempt reservation is missing, completed, or owned by another batch")]
    AttemptOwnership,
    #[error("too many recipe files to reconcile (maximum {MAX_RECONCILE_FILES})")]
    ReconcileLimit,
    #[error("filesystem error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DraftState {
    pub text: String,
    pub diagnostics: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NavigationState {
    pub selected: Option<RecordId>,
    pub anchor: Option<RecordId>,
    pub follow: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StoredBookmark {
    pub record: RecordId,
    pub note: String,
}

/// One persisted colour rule: the predicate exactly as the user typed it, and
/// the colour token. Storing the token rather than an RGB triple keeps the
/// contrast check with the theme, where it can be re-run when the theme or the
/// terminal's colour depth changes.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct StoredColorRule {
    pub predicate: String,
    pub color: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PresentationState {
    /// Ordered working-view sources; empty is the legacy owning source.
    #[serde(default)]
    pub source_ids: Vec<SourceId>,
    #[serde(default)]
    pub bookmarks: Vec<StoredBookmark>,
    /// When this view was last selected, ordering views of one source. Zero
    /// means it has never been chosen, so the source opens on its canonical
    /// view.
    #[serde(default)]
    pub selected_at: u64,
    #[serde(default)]
    pub pinned_columns: Vec<String>,
    #[serde(default)]
    pub color_field: Option<String>,
    /// Ordered predicate colour rules. Additive and `serde(default)` like every
    /// other presentation field, so this needs no `DB_SCHEMA_VERSION` bump: an
    /// older binary reading a newer row ignores the key, and a newer binary
    /// reading an older row gets an empty list. The cost of that choice is that
    /// a *save* by an older binary drops the rules, which is the right trade
    /// for presentation — the alternative refuses the whole workspace on every
    /// downgrade.
    #[serde(default)]
    pub color_rules: Vec<StoredColorRule>,
    /// Repeated-pattern folding. Off unless the user turned it on for this
    /// view; it is reversible presentation, so nothing else depends on it.
    #[serde(default)]
    pub fold_enabled: bool,
    /// None means the built-in minimum run.
    #[serde(default)]
    pub fold_minimum_run: Option<u32>,
    /// The column whose value is the fold key. None — including a view written
    /// before this field existed — means the derived `pattern` column, which is
    /// exactly what folding keyed on before a column could be chosen. The three
    /// fields below are additive: an older reader ignores them and a newer
    /// reader defaults them, so no stored view is rewritten and no schema
    /// version moves.
    #[serde(default)]
    pub fold_key_column: Option<String>,
    /// Rows of other keys one run may span. Zero, and absent, are adjacent-only.
    #[serde(default)]
    pub fold_lookback: u32,
    /// `conservative` / `standard` / `aggressive`; empty and unknown read as
    /// the built-in default. It applies to the derived `pattern` column only.
    #[serde(default)]
    pub fold_normalisation: String,
    /// Folded runs the user expanded, named by their first constituent record.
    #[serde(default)]
    pub fold_expanded: Vec<RecordId>,
    /// Exact typed equality installed by an accepted cross-source correlation.
    #[serde(default)]
    pub exact_field: Option<lvu_core::FieldCorrelation>,
    /// Accepted union inputs for a union view, plus its own search text.
    /// Additive like every other presentation field: an older binary ignores
    /// the key and a newer one defaults it, so no schema bump.
    #[serde(default)]
    pub union: Option<StoredUnion>,
    #[serde(default)]
    pub applied_enrichment: Option<String>,
    /// None means legacy single-stage state. Some([]) is explicitly cleared.
    #[serde(default)]
    pub enrichment_chain: Option<Vec<StoredEnrichment>>,
    /// The single command slot views had before command steps joined the
    /// chain. Read once and migrated into `enrichment_chain` and
    /// `command_steps`; never written by this build.
    #[serde(default)]
    pub command_enrichment: Option<StoredCommandEnrichment>,
    #[serde(default)]
    pub command_enrichment_revision: u64,
    /// Bounded app-owned reference to immutable command result rows.
    #[serde(default)]
    pub command_publication: Option<String>,
    /// Per command step (keyed by stage id): definition revision and the
    /// bounded reference to its last published results.
    #[serde(default)]
    pub command_steps: std::collections::BTreeMap<String, StoredCommandStep>,
    #[serde(default)]
    pub enrichment_editing: Option<String>,
    #[serde(default)]
    pub enrichment_selected: Option<String>,
    #[serde(default)]
    pub enrichment_draft: Option<DraftState>,
    #[serde(default)]
    pub applied_grouping: Option<String>,
    #[serde(default)]
    pub grouping_draft: Option<DraftState>,
    #[serde(default)]
    pub capture_time: Option<TimePolicy>,
    #[serde(default)]
    pub time_basis: crate::TimeBasis,
    /// `lvu_live::TimeFieldSelection::to_token()` for a `Selected` basis.
    #[serde(default)]
    pub time_field: Option<String>,
    #[serde(default)]
    pub capture_time_start_draft: String,
    #[serde(default)]
    pub capture_time_end_draft: String,
    #[serde(default)]
    pub capture_time_error: Option<String>,
    #[serde(default)]
    pub time_draft: Option<StoredTimeDraft>,
    /// Zero means "never set"; the reader substitutes its own default. Storing
    /// the sentinel rather than the resolved value lets the default change
    /// without rewriting stored views.
    #[serde(default)]
    pub time_gap_threshold_seconds: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StoredTimeDraft {
    pub basis: crate::TimeBasis,
    #[serde(default)]
    pub field: Option<String>,
    pub window: StoredTimeWindow,
    pub touched: bool,
    #[serde(default)]
    pub structured_present: bool,
    pub start_date: String,
    pub start_time: String,
    pub start_zone: String,
    pub end_date: String,
    pub end_time: String,
    pub end_zone: String,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StoredTimeWindow {
    #[default]
    All,
    Absolute,
    Recent {
        seconds: u64,
    },
    /// First event to last event, measured against the dataset.
    DataFirstToLast,
    /// The last `seconds` *of data*, measured back from the newest record.
    DataRecent {
        seconds: u64,
    },
    AroundSelected {
        /// Absent in views stored before the width was editable; zero reads as
        /// the built-in default, which is the width they were written with.
        #[serde(default)]
        seconds: u64,
    },
    /// A kind this build does not know, written by a newer one.
    ///
    /// Without this a downgrade would fail to parse the whole presentation
    /// blob and lose a view's bookmarks, pins and colours along with a draft it
    /// merely could not name. Reading it as "no window" is the same choice
    /// `ViewRole::parse_token` makes: an unknown value must not do damage.
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StoredEnrichment {
    pub id: String,
    /// The expression, or a command step's output prefix.
    pub source: String,
    /// Present on a command step: the saved definition, never a result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<lvu_core::CommandDefinition>,
}

/// One accepted union input: the input view ID fenced on revision and
/// generation. Shape-locked with `lvu::PersistentUnionInput` by construction.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StoredUnionInput {
    #[serde(default)]
    pub view_id: String,
    #[serde(default)]
    pub accepted_revision: u64,
    #[serde(default)]
    pub applied_generation: u64,
}

/// A union view's persisted definition: fenced inputs plus its own search.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StoredUnion {
    #[serde(default)]
    pub inputs: Vec<StoredUnionInput>,
    #[serde(default)]
    pub filter: String,
}

/// One command step's run state: its definition revision and the reference
/// to its last published results, if any.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StoredCommandStep {
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub publication: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StoredCommandEnrichment {
    pub id: String,
    pub definition: lvu_core::CommandDefinition,
}

impl PresentationState {
    pub fn effective_enrichments(&self) -> Vec<StoredEnrichment> {
        self.enrichment_chain.clone().unwrap_or_else(|| {
            self.applied_enrichment
                .as_ref()
                .filter(|source| !source.trim().is_empty())
                .map(|source| StoredEnrichment {
                    id: "legacy-enrichment".into(),
                    source: source.clone(),
                    command: None,
                })
                .into_iter()
                .collect()
        })
    }
}

/// Why a view exists, persisted per view and owned by its source.
///
/// This is never derived from the display name: a user may rename any view, and
/// a rename must not change whether its definition can be edited.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ViewRole {
    /// The source's permanent unfiltered view. Its definition is fixed; its
    /// presentation is not.
    Canonical,
    /// An ordinary editable view.
    #[default]
    Derived,
}

impl ViewRole {
    pub fn token(self) -> &'static str {
        match self {
            ViewRole::Canonical => "canonical",
            ViewRole::Derived => "derived",
        }
    }

    /// Unknown tokens read as `Derived`. A role written by a future version
    /// must not accidentally make a view immutable in this one.
    pub fn parse_token(token: &str) -> Self {
        match token {
            "canonical" => ViewRole::Canonical,
            _ => ViewRole::Derived,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct WorkingView {
    pub id: ViewId,
    pub source_id: SourceId,
    pub name: String,
    pub role: ViewRole,
    pub applied_revision_id: Option<Uuid>,
    pub applied_search: String,
    pub search_draft: Option<String>,
    pub applied_advanced_filter: Option<String>,
    pub advanced_filter_draft: Option<DraftState>,
    pub navigation: NavigationState,
    pub presentation: PresentationState,
    pub version: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceMetadata {
    pub definition: SourceDefinition,
    pub project: Option<String>,
    pub command: Option<String>,
    pub fields: BTreeMap<String, String>,
    pub last_seen: i64,
    pub missing: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecipeCandidate {
    pub recipe_id: RecipeId,
    pub revision_id: Uuid,
    pub name: String,
    pub score: i64,
    pub evidence: Vec<String>,
    pub missing_fields: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecipeRevisionSummary {
    pub cursor: i64,
    pub revision_id: Uuid,
    pub content_hash: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SuggestionOutcome {
    Accepted,
    Rejected,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CommandAttemptScope {
    pub view_id: ViewId,
    pub stage_id: String,
    pub command_revision: String,
    pub preceding_definition_revision: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandAttemptReservation {
    pub token: Uuid,
    pub scope: CommandAttemptScope,
    pub record_ids: Vec<RecordId>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum CommandAttemptOutcome {
    Ready {
        fields: BTreeMap<String, serde_json::Value>,
        diagnostic: Option<String>,
    },
    Failed {
        diagnostic: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StoredCommandAttempt {
    NeverAttempted,
    Reserved,
    Ready {
        fields: BTreeMap<String, serde_json::Value>,
        diagnostic: Option<String>,
    },
    Failed {
        diagnostic: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandAttemptRecord {
    pub record_id: RecordId,
    pub state: StoredCommandAttempt,
}

pub struct WorkspaceStore {
    conn: Connection,
    root: PathBuf,
    recipes_root: PathBuf,
}

/// Per-entry results of [`WorkspaceStore::save_sources_and_views`]: the input
/// position with that entry's version or error.
type BatchSaveOutcomes = Vec<(usize, Result<u64, MemoryError>)>;

impl WorkspaceStore {
    pub fn import_legacy_snapshot(
        seed: &LegacyWorkspaceSeed,
    ) -> Result<LegacyImportOutcome, MemoryError> {
        validate_workspace_seed(seed)?;
        fs::create_dir_all(&seed.target_workspace_root).map_err(|source| MemoryError::Io {
            path: seed.target_workspace_root.clone(),
            source,
        })?;
        fs::create_dir_all(&seed.migration_root).map_err(|source| MemoryError::Io {
            path: seed.migration_root.clone(),
            source,
        })?;
        // One admission covers the shared recipe bootstrap and every slot's
        // private import, so no slot observes a half-published seed, namespace
        // or workspace produced by another slot going first.
        let admission = open_admission_lock(&bootstrap_admission_path(
            &seed.migration_root,
            seed.version,
        ))?;
        lock_bounded(&admission, "workspace bootstrap")?;

        let target_db = seed.target_workspace_root.join("workspace.sqlite3");
        if seed.completion_marker.exists() {
            validate_imported_workspace(seed, &target_db)?;
            return Ok(LegacyImportOutcome::AlreadyImported);
        }
        if target_db.exists() {
            validate_imported_workspace(seed, &target_db)?;
            publish_workspace_marker(seed, &target_db)?;
            return Ok(LegacyImportOutcome::AlreadyImported);
        }
        let frozen_seed = RecipeSeed {
            legacy_recipes_root: seed.legacy_workspace_root.join("recipes"),
            target_recipes_root: seed.migration_root.join(".unused-recipes-target"),
            migration_root: seed.migration_root.clone(),
            version: seed.version,
        };
        let (frozen_db, frozen_provenance_path) = frozen_seed_paths(&frozen_seed);
        if !frozen_db.exists() {
            // Seed provenance without its seed means a legacy snapshot was
            // promised to this namespace and is now unreadable. Initialising an
            // empty workspace here would silently drop it.
            if frozen_provenance_path.exists() {
                return Err(MemoryError::InvalidData(
                    "frozen workspace seed provenance exists without its seed".into(),
                ));
            }
            initialize_empty_workspace(seed, &target_db)?;
            return Ok(LegacyImportOutcome::NoLegacyDatabase);
        }
        let source_db = frozen_db;
        let (source_file, frozen_provenance) = open_validated_frozen_seed(&frozen_seed)?;
        // Reopening through the held descriptor binds SQLite to the exact inode
        // that was validated, so a lexical replacement cannot redirect the
        // backup between validation and read.
        #[cfg(target_os = "linux")]
        let source_open_path = {
            use std::os::fd::AsRawFd;
            PathBuf::from(format!("/proc/self/fd/{}", source_file.as_raw_fd()))
        };
        #[cfg(not(target_os = "linux"))]
        let source_open_path = source_db.clone();
        let source = Connection::open_with_flags(
            &source_open_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        source.busy_timeout(Duration::from_secs(2))?;
        let source_schema_version: i64 =
            source.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if source_schema_version > DB_SCHEMA_VERSION {
            return Err(MemoryError::FutureDatabase(source_schema_version));
        }

        let pending = seed
            .migration_root
            .join(format!("workspace-v{}.pending.sqlite3", seed.version));
        match fs::remove_file(&pending) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(MemoryError::Io {
                    path: pending,
                    source,
                });
            }
        }
        let mut destination = Connection::open(&pending)?;
        destination.busy_timeout(Duration::from_secs(2))?;
        let started = std::time::Instant::now();
        {
            let backup = Backup::new(&source, &mut destination)?;
            loop {
                match backup.step(128)? {
                    StepResult::Done => break,
                    StepResult::More
                        if started.elapsed() <= MAX_BACKUP_DURATION
                            && backup.progress().pagecount <= 65_536 => {}
                    StepResult::Busy | StepResult::Locked
                        if started.elapsed() <= MAX_BACKUP_DURATION =>
                    {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    _ => {
                        return Err(MemoryError::InvalidData(
                            "legacy workspace snapshot exceeded its bounded admission".into(),
                        ));
                    }
                }
            }
        }
        if hash_file_handle_bounded(&source_file, MAX_LEGACY_DATABASE_BYTES, &source_db)?
            != frozen_provenance.sha256
        {
            return Err(MemoryError::InvalidData(
                "frozen workspace seed changed during import".into(),
            ));
        }
        migrate_connection(&destination)?;
        let digest = hash_file_bounded(&pending, MAX_LEGACY_DATABASE_BYTES)?;
        let provenance = WorkspaceImportProvenance {
            version: seed.version,
            source_root: stable_path(&seed.legacy_workspace_root)?,
            target_root: stable_path(&seed.target_workspace_root)?,
            source_schema_version,
            initial_snapshot_digest: digest,
        };
        destination.execute_batch(
            "CREATE TABLE workspace_import_provenance(\
                version INTEGER PRIMARY KEY,source_root TEXT NOT NULL,target_root TEXT NOT NULL,\
                source_schema_version INTEGER NOT NULL,initial_snapshot_digest TEXT NOT NULL);",
        )?;
        destination.execute(
            "INSERT INTO workspace_import_provenance VALUES(?1,?2,?3,?4,?5)",
            params![
                provenance.version,
                provenance.source_root,
                provenance.target_root,
                provenance.source_schema_version,
                provenance.initial_snapshot_digest
            ],
        )?;
        let integrity: String =
            destination.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        if integrity != "ok" {
            return Err(MemoryError::InvalidData(format!(
                "imported workspace integrity check failed: {integrity}"
            )));
        }
        destination.close().map_err(|(_, error)| error)?;
        fs::File::open(&pending)
            .and_then(|file| file.sync_all())
            .map_err(|source| MemoryError::Io {
                path: pending.clone(),
                source,
            })?;
        fs::hard_link(&pending, &target_db).map_err(|source| MemoryError::Io {
            path: target_db.clone(),
            source,
        })?;
        sync_directory(&seed.target_workspace_root)?;
        publish_workspace_marker(seed, &target_db)?;
        fs::remove_file(&pending).map_err(|source| MemoryError::Io {
            path: pending,
            source,
        })?;
        Ok(LegacyImportOutcome::Imported)
    }

    pub fn bootstrap_recipe_namespace(
        seed: &RecipeSeed,
    ) -> Result<RecipeBootstrapReport, MemoryError> {
        validate_recipe_seed(seed)?;
        fs::create_dir_all(&seed.migration_root).map_err(|source| MemoryError::Io {
            path: seed.migration_root.clone(),
            source,
        })?;
        let admission = open_admission_lock(&bootstrap_admission_path(
            &seed.migration_root,
            seed.version,
        ))?;
        lock_bounded(&admission, "recipe bootstrap")?;

        if seed.target_recipes_root.exists() {
            return read_embedded_recipe_bootstrap(seed);
        }

        let pending = seed
            .migration_root
            .join(format!("recipes-v{}.pending", seed.version));
        match fs::remove_dir_all(&pending) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(MemoryError::Io {
                    path: pending,
                    source,
                });
            }
        }
        fs::create_dir(&pending).map_err(|source| MemoryError::Io {
            path: pending.clone(),
            source,
        })?;

        let _legacy_lock = acquire_legacy_recipe_lock(&seed.legacy_recipes_root)?;
        freeze_legacy_workspace(seed)?;
        let mut imported = 0usize;
        let mut unavailable = Vec::new();
        let mut files = BTreeMap::new();
        match fs::read_dir(&seed.legacy_recipes_root) {
            Ok(entries) => {
                for entry in entries.take(MAX_RECONCILE_FILES + 1) {
                    if imported + unavailable.len() >= MAX_RECONCILE_FILES {
                        return Err(MemoryError::ReconcileLimit);
                    }
                    let entry = entry.map_err(|source| MemoryError::Io {
                        path: seed.legacy_recipes_root.clone(),
                        source,
                    })?;
                    let path = entry.path();
                    if path.extension().and_then(|value| value.to_str()) != Some("toml") {
                        continue;
                    }
                    let name = entry.file_name().to_string_lossy().into_owned();
                    match read_recipe(&path) {
                        Ok((recipe, hash)) => {
                            let expected_name = format!("{}.toml", recipe.recipe_id.0);
                            if name != expected_name {
                                unavailable.push(RecipeSeedDiagnostic {
                                    file_name: name,
                                    diagnostic: "recipe filename does not match its identity"
                                        .into(),
                                });
                                continue;
                            }
                            let bytes = fs::read(&path).map_err(|source| MemoryError::Io {
                                path: path.clone(),
                                source,
                            })?;
                            if content_hash(&bytes) != hash {
                                return Err(MemoryError::InvalidData(
                                    "legacy recipe changed while it was being imported".into(),
                                ));
                            }
                            let destination = pending.join(expected_name);
                            write_new_synced(&destination, &bytes)?;
                            files.insert(
                                destination
                                    .file_name()
                                    .unwrap()
                                    .to_string_lossy()
                                    .into_owned(),
                                content_hash(&bytes),
                            );
                            imported += 1;
                        }
                        Err(error) => unavailable.push(RecipeSeedDiagnostic {
                            file_name: name,
                            diagnostic: error.to_string(),
                        }),
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(MemoryError::Io {
                    path: seed.legacy_recipes_root.clone(),
                    source,
                });
            }
        }

        let mut revisions = BTreeMap::new();
        stage_legacy_revisions(seed, &pending, &mut revisions, &mut unavailable)?;
        let marker = RecipeBootstrapMarker {
            version: seed.version,
            legacy_recipes_root: stable_path(&seed.legacy_recipes_root)?,
            target_recipes_root: stable_path(&seed.target_recipes_root)?,
            imported,
            unavailable: unavailable.clone(),
            files,
            revisions,
        };
        write_new_synced(
            &pending.join(".bootstrap.json"),
            &serde_json::to_vec(&marker)
                .map_err(|error| MemoryError::InvalidData(error.to_string()))?,
        )?;
        validate_staged_recipe_manifest(&pending, &marker)?;
        sync_directory(&pending)?;
        rename_directory_noreplace(&pending, &seed.target_recipes_root)?;
        sync_directory(seed.target_recipes_root.parent().unwrap())?;
        Ok(RecipeBootstrapReport {
            imported,
            unavailable,
        })
    }

    pub fn list_recipes(&self, limit: u32) -> Result<Vec<(RecipeFile, String)>, MemoryError> {
        if limit == 0 || limit > 128 {
            return Err(MemoryError::InvalidData(
                "recipe list limit must be 1..=128".into(),
            ));
        }
        let _guard = RecipeLock::acquire_in(&self.recipes_root, RecipeId(Uuid::nil()))?;
        self.reconcile_toml_locked()?;
        let mut stmt = self.conn.prepare("SELECT rr.document, rr.content_hash FROM recipes r JOIN recipe_revisions rr ON rr.revision_id=r.current_revision_id ORDER BY r.name,r.recipe_id LIMIT ?1")?;
        let rows = stmt.query_map([limit], |row| {
            Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut values = Vec::new();
        for row in rows {
            let (bytes, hash) = row?;
            if bytes.len() as u64 > crate::MAX_DEFINITION_BYTES {
                return Err(RecipeError::TooLarge.into());
            }
            let text = std::str::from_utf8(&bytes)
                .map_err(|error| RecipeError::Toml(error.to_string()))?;
            let recipe: RecipeFile =
                toml::from_str(text).map_err(|error| RecipeError::Toml(error.to_string()))?;
            recipe.validate()?;
            values.push((recipe, hash));
        }
        Ok(values)
    }

    pub fn save_new_recipe(&mut self, recipe: &RecipeFile) -> Result<SavedRecipe, MemoryError> {
        recipe.validate()?;
        let guard = RecipeLock::acquire_in(&self.recipes_root, recipe.recipe_id)?;
        self.reconcile_toml_locked()?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM recipes WHERE name=?1 OR recipe_id=?2)",
            rusqlite::params![&recipe.name, recipe.recipe_id.0.to_string()],
            |row| row.get(0),
        )?;
        if exists {
            return Err(MemoryError::InvalidData(
                "a recipe with that name or identity already exists".into(),
            ));
        }
        preflight_revision(&tx, recipe)?;
        let saved = save_recipe_locked(&guard, recipe, None)?;
        import_tx(&tx, recipe, &saved.content_hash)?;
        tx.commit()?;
        Ok(saved)
    }

    /// Imports only a new identity/name. Existing recipes require the explicit
    /// optimistic revision API rather than silently moving their current pointer.
    pub fn import_new_recipe(&mut self, path: &Path) -> Result<SavedRecipe, MemoryError> {
        let (recipe, _) = read_recipe(path)?;
        self.save_new_recipe(&recipe)
    }
    pub fn open(root: impl AsRef<Path>) -> Result<Self, MemoryError> {
        let root = root.as_ref();
        Self::open_with_recipes(root, root.join("recipes"))
    }

    pub fn open_with_recipes(
        workspace_root: impl AsRef<Path>,
        recipes_root: impl AsRef<Path>,
    ) -> Result<Self, MemoryError> {
        Self::open_with_roots_and_busy_timeout(workspace_root, recipes_root, Duration::from_secs(2))
    }

    pub fn open_with_busy_timeout(
        root: impl AsRef<Path>,
        busy_timeout: Duration,
    ) -> Result<Self, MemoryError> {
        let root = root.as_ref();
        Self::open_with_roots_and_busy_timeout(root, root.join("recipes"), busy_timeout)
    }

    fn open_with_roots_and_busy_timeout(
        workspace_root: impl AsRef<Path>,
        recipes_root: impl AsRef<Path>,
        busy_timeout: Duration,
    ) -> Result<Self, MemoryError> {
        let root = workspace_root.as_ref().to_path_buf();
        let recipes_root = recipes_root.as_ref().to_path_buf();
        fs::create_dir_all(&root).map_err(|source| MemoryError::Io {
            path: root.clone(),
            source,
        })?;
        let db = root.join("workspace.sqlite3");
        let conn = Connection::open(&db)?;
        conn.set_limit(Limit::SQLITE_LIMIT_LENGTH, MAX_SQLITE_VALUE_BYTES)?;
        conn.busy_timeout(busy_timeout)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version > DB_SCHEMA_VERSION {
            return Err(MemoryError::FutureDatabase(version));
        }
        if version == 0 {
            migrate_v1(&conn)?;
        }
        if version < 2 {
            migrate_v2(&conn)?;
        }
        if version < 3 {
            // Ordered source membership changes the meaning of a working view.
            // Older applications must refuse this database rather than saving
            // a single-source interpretation over the persisted membership.
            conn.pragma_update(None, "user_version", 3)?;
        }
        if version < 4 {
            migrate_v4(&conn)?;
        }
        if version < 5 {
            migrate_v5(&conn)?;
        }
        if version < 6 {
            migrate_v6(&conn)?;
        }
        let store = Self {
            conn,
            root,
            recipes_root,
        };
        store.reconcile_toml()?;
        Ok(store)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn save_recipe(
        &mut self,
        recipe: &RecipeFile,
        expected_hash: Option<&str>,
    ) -> Result<SavedRecipe, MemoryError> {
        recipe.validate()?;
        let guard = RecipeLock::acquire_in(&self.recipes_root, recipe.recipe_id)?;
        self.reconcile_toml_locked()?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        preflight_revision(&tx, recipe)?;
        // Publish canonical TOML before committing its SQLite pointer. A crash
        // in that narrow window is repaired by startup reconciliation.
        let saved = save_recipe_locked(&guard, recipe, expected_hash)?;
        import_tx(&tx, recipe, &saved.content_hash)?;
        tx.commit()?;
        Ok(saved)
    }

    /// Replace configuration only, retaining recipe identity and immutable history.
    /// The selected revision must still be current when the write lock is acquired.
    /// `saved_at` is the caller's clock, as every other timestamped write here
    /// takes it, so a test can write a fixed history and the store stays
    /// deterministic. `None` leaves the revision undated, which reads exactly
    /// as a revision written before the field existed.
    pub fn update_recipe_revision(
        &mut self,
        id: RecipeId,
        expected_revision: Uuid,
        view: &crate::NamedViewDefinition,
        saved_at: Option<i64>,
    ) -> Result<SavedRecipe, MemoryError> {
        let guard = RecipeLock::acquire_in(&self.recipes_root, id)?;
        self.reconcile_toml_locked()?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: Option<(String, String, Vec<u8>)> = tx.query_row(
            "SELECT rr.revision_id,rr.content_hash,rr.document FROM recipes r JOIN recipe_revisions rr ON rr.revision_id=r.current_revision_id WHERE r.recipe_id=?1",
            [id.0.to_string()], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).optional()?;
        let (revision, hash, bytes) = current.ok_or(MemoryError::Conflict)?;
        if revision != expected_revision.to_string() {
            return Err(MemoryError::Conflict);
        }
        let mut recipe = decode_recipe_revision(&bytes, id, expected_revision)?;
        let view_id = recipe.view.id;
        let view_name = recipe.view.name.clone();
        recipe.view = view.clone();
        recipe.view.id = view_id;
        recipe.view.name = view_name;
        recipe.view.source_ids = vec![recipe.source.id];
        recipe.revision_id = Uuid::new_v4();
        // A new revision is a new save, so it carries its own date rather than
        // inheriting the one it was derived from.
        recipe.saved_at_unix_nanos = saved_at;
        recipe.validate()?;
        preflight_revision(&tx, &recipe)?;
        let saved = save_recipe_locked(&guard, &recipe, Some(&hash))?;
        import_tx(&tx, &recipe, &saved.content_hash)?;
        tx.commit()?;
        Ok(saved)
    }

    pub fn recipe_revision_documents(
        &self,
        id: RecipeId,
        limit: u32,
    ) -> Result<Vec<RecipeFile>, MemoryError> {
        if limit == 0 || limit > 100 {
            return Err(MemoryError::InvalidData(
                "history limit must be 1..=100".into(),
            ));
        }
        let _guard = RecipeLock::acquire_in(&self.recipes_root, RecipeId(Uuid::nil()))?;
        self.reconcile_toml_locked()?;
        self.recipe_history_db(id, None, limit)?
            .into_iter()
            .map(|revision| {
                let bytes: Vec<u8> = self.conn.query_row(
                    "SELECT document FROM recipe_revisions WHERE recipe_id=?1 AND revision_id=?2",
                    params![id.0.to_string(), revision.revision_id.to_string()],
                    |row| row.get(0),
                )?;
                decode_recipe_revision(&bytes, id, revision.revision_id)
            })
            .collect()
    }

    /// Installs an external recipe into the application-owned canonical path.
    pub fn import_recipe(
        &mut self,
        path: &Path,
        expected_hash: Option<&str>,
    ) -> Result<SavedRecipe, MemoryError> {
        let (recipe, _) = read_recipe(path)?;
        self.save_recipe(&recipe, expected_hash)
    }

    pub fn reconcile_toml(&self) -> Result<(), MemoryError> {
        let _guard = RecipeLock::acquire_in(&self.recipes_root, RecipeId(Uuid::nil()))?;
        self.reconcile_toml_locked()
    }

    fn reconcile_toml_locked(&self) -> Result<(), MemoryError> {
        let dir = self.recipes_root.clone();
        match fs::symlink_metadata(&dir) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(RecipeError::UnsafePath.into());
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(source) => return Err(MemoryError::Io { path: dir, source }),
        }
        let entries = match fs::read_dir(&dir) {
            Ok(v) => v,
            Err(source) => return Err(MemoryError::Io { path: dir, source }),
        };
        let mut paths = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| MemoryError::Io {
                path: dir.clone(),
                source,
            })?;
            if entry
                .file_type()
                .map_err(|source| MemoryError::Io {
                    path: entry.path(),
                    source,
                })?
                .is_file()
                && entry.path().extension().is_some_and(|v| v == "toml")
            {
                paths.push(entry.path());
            }
            if paths.len() > MAX_RECONCILE_FILES {
                return Err(MemoryError::ReconcileLimit);
            }
        }
        paths.sort();
        // Validate every authoritative file before changing SQLite. A future or
        // malformed file therefore leaves all existing working state untouched.
        let parsed = paths
            .iter()
            .map(|path| read_recipe(path))
            .collect::<Result<Vec<_>, _>>()?;
        let current: BTreeSet<String> = parsed
            .iter()
            .map(|(recipe, _)| recipe.revision_id.to_string())
            .collect();
        let tx = self.conn.unchecked_transaction()?;
        // Shared immutable history first, then the current pointers. Importing
        // the pointer last keeps the newest revision at the head of this
        // catalog's history even when another slot published it.
        self.import_shared_revisions(&tx, &current)?;
        for (recipe, hash) in &parsed {
            import_tx(&tx, recipe, hash)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Imports every bounded shared immutable revision document that this
    /// catalog has not seen. Documents are immutable, so an already-recorded
    /// revision id is skipped without reading the file.
    fn import_shared_revisions(
        &self,
        tx: &Transaction<'_>,
        current: &BTreeSet<String>,
    ) -> Result<(), MemoryError> {
        let root = shared_revisions_root(&self.recipes_root);
        match fs::symlink_metadata(&root) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(RecipeError::UnsafePath.into());
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(source) => return Err(MemoryError::Io { path: root, source }),
        }
        let mut known =
            tx.prepare("SELECT EXISTS(SELECT 1 FROM recipe_revisions WHERE revision_id=?1)")?;
        let mut total_bytes = 0u64;
        let mut directories = 0usize;
        let entries = fs::read_dir(&root).map_err(|source| MemoryError::Io {
            path: root.clone(),
            source,
        })?;
        for entry in entries {
            let entry = entry.map_err(|source| MemoryError::Io {
                path: root.clone(),
                source,
            })?;
            if !entry
                .file_type()
                .map_err(|source| MemoryError::Io {
                    path: entry.path(),
                    source,
                })?
                .is_dir()
            {
                continue;
            }
            directories += 1;
            if directories > MAX_RECONCILE_FILES {
                return Err(MemoryError::ReconcileLimit);
            }
            let Some(recipe_id) = entry
                .file_name()
                .to_str()
                .and_then(|name| Uuid::parse_str(name).ok())
            else {
                continue;
            };
            let directory = entry.path();
            let mut names = Vec::new();
            for revision in fs::read_dir(&directory).map_err(|source| MemoryError::Io {
                path: directory.clone(),
                source,
            })? {
                let revision = revision.map_err(|source| MemoryError::Io {
                    path: directory.clone(),
                    source,
                })?;
                let path = revision.path();
                if path.extension().and_then(|value| value.to_str()) != Some("toml") {
                    continue;
                }
                let Some(revision_id) = path
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .and_then(|value| Uuid::parse_str(value).ok())
                else {
                    // Interrupted publications leave dotted temporaries. They
                    // never occupy a final name and are not history.
                    continue;
                };
                names.push((revision_id, path));
                if names.len() > MAX_SHARED_REVISIONS_PER_RECIPE {
                    return Err(MemoryError::ReconcileLimit);
                }
            }
            names.sort_by_key(|(revision_id, _)| *revision_id);
            for (revision_id, path) in names {
                let text = revision_id.to_string();
                if current.contains(&text)
                    || known.query_row([&text], |row| row.get::<_, bool>(0))?
                {
                    continue;
                }
                let (recipe, _) = read_recipe(&path)?;
                if recipe.recipe_id.0 != recipe_id || recipe.revision_id != revision_id {
                    return Err(MemoryError::InvalidData(
                        "shared revision document does not match its published identity".into(),
                    ));
                }
                total_bytes += fs::metadata(&path)
                    .map_err(|source| MemoryError::Io {
                        path: path.clone(),
                        source,
                    })?
                    .len();
                if total_bytes > MAX_SHARED_REVISION_BYTES {
                    return Err(MemoryError::ReconcileLimit);
                }
                import_revision_tx(tx, &recipe)?;
            }
        }
        Ok(())
    }

    pub fn upsert_source(&self, source: &SourceMetadata) -> Result<(), MemoryError> {
        validate_source(&source.definition)?;
        self.conn.execute("INSERT INTO sources(source_id,definition_json,project,command,fields_json,last_seen,missing) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(source_id) DO UPDATE SET definition_json=excluded.definition_json,project=COALESCE(excluded.project,sources.project),command=COALESCE(excluded.command,sources.command),fields_json=CASE WHEN excluded.fields_json=x'7b7d' THEN sources.fields_json ELSE excluded.fields_json END,last_seen=excluded.last_seen,missing=excluded.missing", params![source.definition.id.0.to_string(), serde_json::to_vec(&source.definition).map_err(invalid)?, source.project, source.command, serde_json::to_vec(&source.fields).map_err(invalid)?, source.last_seen, source.missing])?;
        Ok(())
    }

    pub fn attach_fingerprint(
        &self,
        source_id: SourceId,
        kind: &str,
        value: &str,
    ) -> Result<(), MemoryError> {
        self.conn.execute(
            "INSERT OR IGNORE INTO source_fingerprints(source_id,kind,value) VALUES(?1,?2,?3)",
            params![source_id.0.to_string(), kind, value],
        )?;
        Ok(())
    }

    pub fn source_by_fingerprint(
        &self,
        kind: &str,
        value: &str,
    ) -> Result<Option<SourceId>, MemoryError> {
        self.conn
            .query_row(
                "SELECT source_id FROM source_fingerprints WHERE kind=?1 AND value=?2 ORDER BY source_id LIMIT 1",
                params![kind, value],
                |row| Ok(SourceId(parse_uuid(row.get(0)?)?)),
            )
            .optional()
            .map_err(MemoryError::from)
    }

    pub fn recent_sources(
        &self,
        before: Option<(i64, SourceId)>,
        limit: u32,
    ) -> Result<Vec<SourceMetadata>, MemoryError> {
        check_limit(limit)?;
        let (time, id) = before
            .map(|(t, id)| (t, id.0.to_string()))
            .unwrap_or((i64::MAX, "ffffffff-ffff-ffff-ffff-ffffffffffff".into()));
        let mut stmt = self.conn.prepare("SELECT definition_json,project,command,fields_json,last_seen,missing FROM sources WHERE (last_seen<?1 OR (last_seen=?1 AND source_id<?2)) ORDER BY last_seen DESC,source_id DESC LIMIT ?3")?;
        let rows = stmt.query_map(params![time, id, limit], source_row)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(MemoryError::from)
    }

    /// Persists a view. Its bookmarks are written to their sources, not into
    /// the view row, so no view can hold a private copy that later diverges.
    pub fn create_view(&self, view: &WorkingView) -> Result<(), MemoryError> {
        validate_working_view(view)?;
        self.conn.execute("INSERT INTO working_views(view_id,source_id,name,applied_revision_id,applied_search,search_draft,applied_advanced_filter,advanced_filter_draft_json,navigation_json,version,presentation_json,role) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)", params![view.id.0.to_string(), view.source_id.0.to_string(), view.name, view.applied_revision_id.map(|v|v.to_string()), view.applied_search, view.search_draft, view.applied_advanced_filter, json_opt(&view.advanced_filter_draft)?, serde_json::to_vec(&view.navigation).map_err(invalid)?, to_i64(view.version)?,stored_presentation(&self.conn, view)?, view.role.token()])?;
        write_source_bookmarks(
            &self.conn,
            &view_sources(view),
            &view.presentation.bookmarks,
        )?;
        Ok(())
    }

    pub fn update_view(
        &self,
        view: &WorkingView,
        expected_version: u64,
    ) -> Result<u64, MemoryError> {
        validate_working_view(view)?;
        let next = expected_version
            .checked_add(1)
            .ok_or_else(|| MemoryError::InvalidData("view version overflow".into()))?;
        let changed = self.conn.execute("UPDATE working_views SET name=?2,applied_revision_id=?3,applied_search=?4,search_draft=?5,applied_advanced_filter=?6,advanced_filter_draft_json=?7,navigation_json=?8,version=?9,presentation_json=?11 WHERE view_id=?1 AND version=?10", params![view.id.0.to_string(), view.name, view.applied_revision_id.map(|v|v.to_string()), view.applied_search, view.search_draft, view.applied_advanced_filter, json_opt(&view.advanced_filter_draft)?, serde_json::to_vec(&view.navigation).map_err(invalid)?, to_i64(next)?, to_i64(expected_version)?,stored_presentation(&self.conn, view)?])?;
        if changed != 1 {
            return Err(MemoryError::Conflict);
        }
        write_source_bookmarks(
            &self.conn,
            &view_sources(view),
            &view.presentation.bookmarks,
        )?;
        Ok(next)
    }

    pub fn get_view(&self, id: ViewId) -> Result<Option<WorkingView>, MemoryError> {
        let value = self.conn.query_row("SELECT source_id,name,applied_revision_id,applied_search,search_draft,applied_advanced_filter,advanced_filter_draft_json,navigation_json,version,presentation_json,role FROM working_views WHERE view_id=?1", [id.0.to_string()], |r| {
            let version: i64 = r.get(8)?;
            Ok(WorkingView { id, source_id: SourceId(parse_uuid(r.get::<_,String>(0)?)?), name:r.get(1)?, role: ViewRole::parse_token(&r.get::<_,String>(10)?), applied_revision_id:r.get::<_,Option<String>>(2)?.map(parse_uuid).transpose()?, applied_search:r.get(3)?, search_draft:r.get(4)?, applied_advanced_filter:r.get(5)?, advanced_filter_draft:from_json_opt(r.get(6)?)?, navigation: serde_json::from_slice(&r.get::<_,Vec<u8>>(7)?).map_err(sql_invalid)?, version:u64::try_from(version).map_err(|e|rusqlite::Error::FromSqlConversionFailure(8,rusqlite::types::Type::Integer,Box::new(e)))?, presentation: serde_json::from_slice(&r.get::<_,Vec<u8>>(9)?).map_err(sql_invalid)? })
        }).optional().map_err(MemoryError::from)?;
        // Bookmarks live with their source, so every view of that source shows
        // the same set however it was filtered.
        let value = match value {
            Some(mut view) => {
                view.presentation.bookmarks =
                    read_source_bookmarks(&self.conn, &view_sources(&view))?;
                validate_working_view(&view)?;
                Some(view)
            }
            None => None,
        };
        Ok(value)
    }

    pub fn working_view_for_source(
        &self,
        source_id: SourceId,
    ) -> Result<Option<WorkingView>, MemoryError> {
        let id: Option<String> = self
            .conn
            .query_row(
                "SELECT view_id FROM working_views WHERE source_id=?1 ORDER BY view_id LIMIT 1",
                [source_id.0.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        id.map(|value| {
            let uuid = Uuid::parse_str(&value)
                .map_err(|error| MemoryError::InvalidData(error.to_string()))?;
            self.get_view(ViewId(uuid))?
                .ok_or_else(|| MemoryError::InvalidData("working view disappeared".into()))
        })
        .transpose()
    }

    pub fn working_views_for_source(
        &self,
        source_id: SourceId,
        limit: u32,
    ) -> Result<Vec<WorkingView>, MemoryError> {
        check_limit(limit)?;
        let mut statement = self.conn.prepare(
            "SELECT view_id FROM working_views WHERE source_id=?1 ORDER BY name,view_id LIMIT ?2",
        )?;
        let ids = statement
            .query_map(params![source_id.0.to_string(), limit], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        ids.into_iter()
            .map(|value| {
                let id = ViewId(parse_uuid(value)?);
                self.get_view(id)?
                    .ok_or_else(|| MemoryError::InvalidData("working view disappeared".into()))
            })
            .collect()
    }

    /// Returns the source's canonical view, creating it when the source has
    /// none.
    ///
    /// An existing view is only ever reused when its own persisted role already
    /// says `canonical`. A view that predates roles, or that a user has since
    /// filtered or renamed, stays exactly as it is and a new canonical view is
    /// created beside it, so migration cannot destroy a working definition.
    pub fn ensure_canonical_view(
        &self,
        source_id: SourceId,
        preferred_id: ViewId,
        name: &str,
    ) -> Result<WorkingView, MemoryError> {
        if let Some(existing) = self.canonical_view_for_source(source_id)? {
            return Ok(existing);
        }
        if name.is_empty() || name.len() > MAX_VIEW_NAME_BYTES {
            return Err(MemoryError::InvalidData(
                "invalid canonical view name".into(),
            ));
        }
        let mut candidate = preferred_id;
        for _ in 0..MAX_CANONICAL_ID_ATTEMPTS {
            if self.view_id_is_free(candidate)? {
                let view = WorkingView {
                    id: candidate,
                    source_id,
                    name: name.to_owned(),
                    role: ViewRole::Canonical,
                    applied_revision_id: None,
                    applied_search: String::new(),
                    search_draft: None,
                    applied_advanced_filter: None,
                    advanced_filter_draft: None,
                    navigation: NavigationState {
                        selected: None,
                        anchor: None,
                        follow: true,
                    },
                    presentation: PresentationState::default(),
                    version: 0,
                };
                self.create_view(&view)?;
                return Ok(view);
            }
            // The preferred identity is already taken by some other view.
            // Step to a further deterministic identity rather than adopting
            // that view, which would be exactly the destructive reuse the
            // migration must avoid.
            let mut bytes = *candidate.0.as_bytes();
            let last = bytes.len() - 1;
            bytes[last] = bytes[last].wrapping_add(1);
            candidate = ViewId(Uuid::from_bytes(bytes));
        }
        Err(MemoryError::InvalidData(
            "could not allocate a canonical view identity".into(),
        ))
    }

    /// The source's canonical view, by persisted role only.
    pub fn canonical_view_for_source(
        &self,
        source_id: SourceId,
    ) -> Result<Option<WorkingView>, MemoryError> {
        let id: Option<String> = self
            .conn
            .query_row(
                "SELECT view_id FROM working_views WHERE source_id=?1 AND role=?2 \
                 ORDER BY view_id LIMIT 1",
                params![source_id.0.to_string(), ViewRole::Canonical.token()],
                |row| row.get(0),
            )
            .optional()?;
        id.map(|value| {
            let id = ViewId(parse_uuid(value).map_err(MemoryError::from)?);
            self.get_view(id)?
                .ok_or_else(|| MemoryError::InvalidData("canonical view disappeared".into()))
        })
        .transpose()
    }

    fn view_id_is_free(&self, id: ViewId) -> Result<bool, MemoryError> {
        let existing: Option<String> = self
            .conn
            .query_row(
                "SELECT view_id FROM working_views WHERE view_id=?1",
                [id.0.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        Ok(existing.is_none())
    }

    /// Saves a view's definition and presentation.
    ///
    /// The role is deliberately absent from both the insert and the update: it
    /// is written once when the row is created, so ordinary autosave can never
    /// promote a view to canonical or demote the canonical one.
    pub fn save_source_and_view(
        &mut self,
        source: &SourceMetadata,
        view: &WorkingView,
        expected_version: Option<u64>,
    ) -> Result<u64, MemoryError> {
        validate_source(&source.definition)?;
        validate_working_view(view)?;
        if source.definition.id != view.source_id {
            return Err(MemoryError::InvalidData(
                "view/source identity mismatch".into(),
            ));
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("INSERT INTO sources(source_id,definition_json,project,command,fields_json,last_seen,missing) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(source_id) DO UPDATE SET definition_json=excluded.definition_json,project=excluded.project,command=excluded.command,fields_json=excluded.fields_json,last_seen=excluded.last_seen,missing=excluded.missing", params![source.definition.id.0.to_string(), serde_json::to_vec(&source.definition).map_err(invalid)?, source.project, source.command, serde_json::to_vec(&source.fields).map_err(invalid)?, source.last_seen, source.missing])?;
        let version = match expected_version {
            None => {
                tx.execute("INSERT INTO working_views(view_id,source_id,name,applied_revision_id,applied_search,search_draft,applied_advanced_filter,advanced_filter_draft_json,navigation_json,version,presentation_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,0,?10)", params![view.id.0.to_string(),view.source_id.0.to_string(),view.name,view.applied_revision_id.map(|v|v.to_string()),view.applied_search,view.search_draft,view.applied_advanced_filter,json_opt(&view.advanced_filter_draft)?,serde_json::to_vec(&view.navigation).map_err(invalid)?,stored_presentation(&tx, view)?])?;
                0
            }
            Some(expected) => {
                let next = expected
                    .checked_add(1)
                    .ok_or_else(|| MemoryError::InvalidData("view version overflow".into()))?;
                let changed=tx.execute("UPDATE working_views SET name=?2,applied_revision_id=?3,applied_search=?4,search_draft=?5,applied_advanced_filter=?6,advanced_filter_draft_json=?7,navigation_json=?8,version=?9,presentation_json=?11 WHERE view_id=?1 AND version=?10",params![view.id.0.to_string(),view.name,view.applied_revision_id.map(|v|v.to_string()),view.applied_search,view.search_draft,view.applied_advanced_filter,json_opt(&view.advanced_filter_draft)?,serde_json::to_vec(&view.navigation).map_err(invalid)?,to_i64(next)?,to_i64(expected)?,stored_presentation(&tx, view)?])?;
                if changed != 1 {
                    return Err(MemoryError::Conflict);
                }
                next
            }
        };
        write_source_bookmarks(&tx, &view_sources(view), &view.presentation.bookmarks)?;
        tx.commit()?;
        Ok(version)
    }

    /// Persists several source/view pairs in one `BEGIN IMMEDIATE` transaction.
    ///
    /// Each entry keeps the serial save's all-or-nothing scope — its source
    /// upsert, view insert/update with optimistic-concurrency check, and
    /// bookmark replacement commit or roll back together via a savepoint —
    /// so one entry's failure is reported per entry without changing what
    /// the others made durable. Bookmarks keep the serial last-writer-wins
    /// rule in batch order. A duplicate view id in one batch is refused as
    /// a conflict. Callers should pass at most one entry per view.
    pub fn save_sources_and_views(
        &mut self,
        items: &[(SourceMetadata, WorkingView, Option<u64>)],
    ) -> Vec<Result<u64, MemoryError>> {
        let mut results: Vec<Result<u64, MemoryError>> = Vec::with_capacity(items.len());
        let mut valid: Vec<usize> = Vec::with_capacity(items.len());
        let mut seen_views: BTreeSet<String> = BTreeSet::new();
        for (index, (source, view, _)) in items.iter().enumerate() {
            let outcome = validate_source(&source.definition)
                .map_err(MemoryError::from)
                .and_then(|()| validate_working_view(view))
                .and_then(|()| {
                    if source.definition.id != view.source_id {
                        Err(MemoryError::InvalidData(
                            "view/source identity mismatch".into(),
                        ))
                    } else if !seen_views.insert(view.id.0.to_string()) {
                        Err(MemoryError::Conflict)
                    } else {
                        Ok(())
                    }
                });
            match outcome {
                Ok(()) => {
                    results.push(Ok(0));
                    valid.push(index);
                }
                Err(error) => results.push(Err(error)),
            }
        }
        if valid.is_empty() {
            return results;
        }
        let commit: Result<BatchSaveOutcomes, MemoryError> = (|| {
            let tx = self
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let mut versions: BatchSaveOutcomes = Vec::new();
            for (slot, index) in valid.iter().enumerate() {
                let (source, view, expected) = &items[*index];
                // One savepoint per entry: the serial save rolled back the
                // view write, its source upsert and its bookmark replacement
                // together on any failure, so the batch must be able to undo
                // exactly this entry — including a bookmark write that fails
                // after the view row was already changed — while committing
                // the rest. Savepoint mechanics failing means the transaction
                // itself is unusable, so those errors abort the whole batch;
                // entry-body errors roll back to the savepoint and are
                // reported per entry.
                tx.execute_batch(&format!("SAVEPOINT batch_view_{slot}"))?;
                let body: Result<u64, MemoryError> = (|| {
                    tx.execute("INSERT INTO sources(source_id,definition_json,project,command,fields_json,last_seen,missing) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(source_id) DO UPDATE SET definition_json=excluded.definition_json,project=excluded.project,command=excluded.command,fields_json=excluded.fields_json,last_seen=excluded.last_seen,missing=excluded.missing", params![source.definition.id.0.to_string(), serde_json::to_vec(&source.definition).map_err(invalid)?, source.project, source.command, serde_json::to_vec(&source.fields).map_err(invalid)?, source.last_seen, source.missing])?;
                    let version = match expected {
                        None => {
                            let inserted = tx.execute("INSERT INTO working_views(view_id,source_id,name,applied_revision_id,applied_search,search_draft,applied_advanced_filter,advanced_filter_draft_json,navigation_json,version,presentation_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,0,?10)", params![view.id.0.to_string(),view.source_id.0.to_string(),view.name,view.applied_revision_id.map(|v|v.to_string()),view.applied_search,view.search_draft,view.applied_advanced_filter,json_opt(&view.advanced_filter_draft)?,serde_json::to_vec(&view.navigation).map_err(invalid)?,stored_presentation(&tx, view)?])?;
                            if inserted != 1 {
                                return Err(MemoryError::Conflict);
                            }
                            0
                        }
                        Some(expected) => {
                            let next = expected.checked_add(1).ok_or_else(|| {
                                MemoryError::InvalidData("view version overflow".into())
                            })?;
                            let changed=tx.execute("UPDATE working_views SET name=?2,applied_revision_id=?3,applied_search=?4,search_draft=?5,applied_advanced_filter=?6,advanced_filter_draft_json=?7,navigation_json=?8,version=?9,presentation_json=?11 WHERE view_id=?1 AND version=?10",params![view.id.0.to_string(),view.name,view.applied_revision_id.map(|v|v.to_string()),view.applied_search,view.search_draft,view.applied_advanced_filter,json_opt(&view.advanced_filter_draft)?,serde_json::to_vec(&view.navigation).map_err(invalid)?,to_i64(next)?,to_i64(*expected)?,stored_presentation(&tx, view)?])?;
                            if changed != 1 {
                                return Err(MemoryError::Conflict);
                            }
                            next
                        }
                    };
                    write_source_bookmarks(&tx, &view_sources(view), &view.presentation.bookmarks)?;
                    Ok(version)
                })();
                match body {
                    Ok(version) => {
                        tx.execute_batch(&format!("RELEASE batch_view_{slot}"))?;
                        versions.push((*index, Ok(version)));
                    }
                    Err(error) => {
                        tx.execute_batch(&format!(
                            "ROLLBACK TO batch_view_{slot}; RELEASE batch_view_{slot}"
                        ))?;
                        versions.push((*index, Err(error)));
                    }
                }
            }
            tx.commit()?;
            Ok(versions)
        })();
        match commit {
            Ok(done) => {
                for (index, outcome) in done {
                    results[index] = outcome;
                }
            }
            Err(error) => {
                // A transaction-level failure (BEGIN, savepoint mechanics,
                // commit) rolls everything back: nothing in this batch is
                // durable, so every view that was not already refused for its
                // own data fails with the same cause rather than reporting a
                // partial commit. Entry-body failures above are already
                // per-entry and stay as they are.
                let message = error.to_string();
                for index in valid {
                    if results[index].is_ok() {
                        results[index] = Err(MemoryError::InvalidData(message.clone()));
                    }
                }
            }
        }
        results
    }

    pub fn recipe_history(
        &self,
        id: RecipeId,
        before_rowid: Option<i64>,
        limit: u32,
    ) -> Result<Vec<RecipeRevisionSummary>, MemoryError> {
        let _guard = RecipeLock::acquire_in(&self.recipes_root, RecipeId(Uuid::nil()))?;
        self.reconcile_toml_locked()?;
        self.recipe_history_db(id, before_rowid, limit)
    }

    fn recipe_history_db(
        &self,
        id: RecipeId,
        before_rowid: Option<i64>,
        limit: u32,
    ) -> Result<Vec<RecipeRevisionSummary>, MemoryError> {
        check_limit(limit)?;
        let before = before_rowid.unwrap_or(i64::MAX);
        let mut stmt = self.conn.prepare("SELECT rowid,revision_id,content_hash FROM recipe_revisions WHERE recipe_id=?1 AND rowid<?2 ORDER BY rowid DESC LIMIT ?3")?;
        let rows = stmt.query_map(params![id.0.to_string(), before, limit], |r| {
            Ok(RecipeRevisionSummary {
                cursor: r.get(0)?,
                revision_id: parse_uuid(r.get(1)?)?,
                content_hash: r.get(2)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(MemoryError::from)
    }

    /// Export the exact reviewed immutable revision, even if the current pointer changes.
    pub fn export_recipe_revision(
        &self,
        recipe: RecipeId,
        revision: Uuid,
        path: &Path,
    ) -> Result<SavedRecipe, MemoryError> {
        let _guard = RecipeLock::acquire_in(&self.recipes_root, RecipeId(Uuid::nil()))?;
        self.reconcile_toml_locked()?;
        let document: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT document FROM recipe_revisions WHERE recipe_id=?1 AND revision_id=?2",
                params![recipe.0.to_string(), revision.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        let bytes = document
            .ok_or_else(|| MemoryError::InvalidData("revision does not belong to recipe".into()))?;
        if bytes.len() as u64 > crate::MAX_DEFINITION_BYTES {
            return Err(RecipeError::TooLarge.into());
        }
        let text =
            std::str::from_utf8(&bytes).map_err(|error| RecipeError::Toml(error.to_string()))?;
        let definition: RecipeFile =
            toml::from_str(text).map_err(|error| RecipeError::Toml(error.to_string()))?;
        if definition.recipe_id != recipe || definition.revision_id != revision {
            return Err(RecipeError::Conflict.into());
        }
        Ok(crate::export_recipe(path, &definition)?)
    }

    pub fn restore_recipe_revision(
        &mut self,
        recipe: RecipeId,
        revision: Uuid,
        expected_file_hash: &str,
    ) -> Result<SavedRecipe, MemoryError> {
        let guard = RecipeLock::acquire_in(&self.recipes_root, recipe)?;
        self.reconcile_toml_locked()?;
        let document: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT document FROM recipe_revisions WHERE recipe_id=?1 AND revision_id=?2",
                params![recipe.0.to_string(), revision.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        let document = document
            .ok_or_else(|| MemoryError::InvalidData("revision does not belong to recipe".into()))?;
        let text =
            std::str::from_utf8(&document).map_err(|e| MemoryError::InvalidData(e.to_string()))?;
        let definition: RecipeFile =
            toml::from_str(text).map_err(|e| MemoryError::InvalidData(e.to_string()))?;
        definition.validate()?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        preflight_revision(&tx, &definition)?;
        let saved = save_recipe_locked(&guard, &definition, Some(expected_file_hash))?;
        import_tx(&tx, &definition, &saved.content_hash)?;
        tx.commit()?;
        Ok(saved)
    }

    pub fn record_usage(
        &self,
        source: SourceId,
        recipe: RecipeId,
        at: i64,
    ) -> Result<(), MemoryError> {
        self.conn.execute("INSERT INTO source_recipe_usage(source_id,recipe_id,use_count,last_used) VALUES(?1,?2,1,?3) ON CONFLICT(source_id,recipe_id) DO UPDATE SET use_count=use_count+1,last_used=excluded.last_used",params![source.0.to_string(),recipe.0.to_string(),at])?;
        Ok(())
    }
    pub fn record_suggestion(
        &self,
        source: SourceId,
        recipe: RecipeId,
        revision: Uuid,
        outcome: SuggestionOutcome,
        at: i64,
    ) -> Result<(), MemoryError> {
        self.conn.execute("INSERT INTO suggestion_outcomes(source_id,recipe_id,revision_id,outcome,recorded_at) VALUES(?1,?2,?3,?4,?5)",params![source.0.to_string(),recipe.0.to_string(),revision.to_string(),match outcome {SuggestionOutcome::Accepted=>"accepted",SuggestionOutcome::Rejected=>"rejected"},at])?;
        Ok(())
    }

    pub fn candidates(
        &self,
        source: SourceId,
        project: Option<&str>,
        command: Option<&str>,
        fields: &BTreeMap<String, String>,
        limit: u32,
    ) -> Result<Vec<RecipeCandidate>, MemoryError> {
        let _guard = RecipeLock::acquire_in(&self.recipes_root, RecipeId(Uuid::nil()))?;
        self.reconcile_toml_locked()?;
        check_limit(limit)?;
        let target_definition = self
            .conn
            .query_row(
                "SELECT definition_json FROM sources WHERE source_id=?1",
                [source.0.to_string()],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()?
            .and_then(|bytes| serde_json::from_slice::<SourceDefinition>(&bytes).ok());
        let target_family = target_definition.as_ref().map(source_family);
        let mut stmt=self.conn.prepare("SELECT r.recipe_id,r.current_revision_id,r.name,s.project,s.command,s.fields_json,s.definition_json,COALESCE(u.use_count,0),COALESCE(u.last_used,0),COALESCE((SELECT SUM(CASE outcome WHEN 'accepted' THEN 1 ELSE -1 END) FROM suggestion_outcomes o WHERE o.source_id=?1 AND o.recipe_id=r.recipe_id),0) FROM recipes r JOIN sources s ON s.source_id=r.source_id LEFT JOIN source_recipe_usage u ON u.source_id=?1 AND u.recipe_id=r.recipe_id ORDER BY (s.project=?2) DESC,(s.command=?3) DESC,COALESCE(u.use_count,0) DESC,r.recipe_id LIMIT ?4")?;
        let mut candidates = Vec::new();
        let rows = stmt.query_map(
            params![source.0.to_string(), project, command, MAX_CANDIDATE_SCAN],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, Vec<u8>>(5)?,
                    r.get::<_, Vec<u8>>(6)?,
                    r.get::<_, i64>(7)?,
                    r.get::<_, i64>(8)?,
                    r.get::<_, i64>(9)?,
                ))
            },
        )?;
        for row in rows {
            let (rid, rev, name, p, c, fjson, definition_json, uses, last, outcome) = row?;
            let candidate_fields: BTreeMap<String, String> =
                serde_json::from_slice(&fjson).map_err(invalid)?;
            let mut evidence = Vec::new();
            let mut score = uses.min(20) + outcome * 20;
            if outcome > 0 {
                evidence.push(format!("net {outcome} prior acceptance signal"));
            } else if outcome < 0 {
                evidence.push(format!("net {} prior rejection signal", -outcome));
            }
            if project.is_some() && project == p.as_deref() {
                score += 100;
                evidence.push("same project".into());
            }
            if command.is_some() && command == c.as_deref() {
                score += 80;
                evidence.push("same command".into());
            }
            if let (Some(target), Ok(candidate)) = (
                target_family.as_ref(),
                serde_json::from_slice::<SourceDefinition>(&definition_json),
            ) && *target == source_family(&candidate)
            {
                score += 40;
                evidence.push(format!("same {target} source family"));
            }
            let exact = fields
                .iter()
                .filter(|(k, v)| candidate_fields.get(*k) == Some(*v))
                .count() as i64;
            let names = fields
                .keys()
                .filter(|k| candidate_fields.contains_key(*k))
                .count() as i64;
            score += exact * 10 + (names - exact) * 3;
            if exact > 0 && fields.values().all(|value| value == "display-text") {
                evidence.push(format!(
                    "{exact} field names observed in sampled visible rows"
                ));
            } else if exact > 0 {
                evidence.push(format!("{exact} matching authoritative field types"));
            } else if names > 0 {
                evidence.push(format!("{names} matching field names"));
            }
            let missing_fields = candidate_fields
                .keys()
                .filter(|field| !fields.contains_key(*field))
                .take(32)
                .cloned()
                .collect();
            if uses > 0 {
                evidence.push(format!("used {uses} times on this source"));
            }
            if last > 0 {
                evidence.push(format!("last used {last}"));
            }
            // A shared acquisition family is the minimum reviewable evidence;
            // unrelated acquisition kinds with no other signal are omitted.
            if score < 40 {
                continue;
            }
            candidates.push(RecipeCandidate {
                recipe_id: RecipeId(parse_uuid(rid)?),
                revision_id: parse_uuid(rev)?,
                name,
                score,
                evidence,
                missing_fields,
            });
        }
        candidates.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| a.recipe_id.cmp(&b.recipe_id))
        });
        candidates.truncate(limit as usize);
        Ok(candidates)
    }

    /// Admission checks do not need to materialize previously stored results.
    pub fn has_command_attempt(
        &self,
        scope: &CommandAttemptScope,
        id: RecordId,
    ) -> Result<bool, MemoryError> {
        validate_attempt_scope(scope)?;
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM command_attempts WHERE view_id=?1 AND stage_id=?2 AND command_revision=?3 AND preceding_definition_revision=?4 AND source_id=?5 AND sequence=?6)",
            params![scope.view_id.0.to_string(), scope.stage_id, scope.command_revision, scope.preceding_definition_revision, id.source_id.0.to_string(), id.sequence.to_string()],
            |row| row.get(0),
        )?)
    }

    pub fn command_attempt_count(&self, scope: &CommandAttemptScope) -> Result<usize, MemoryError> {
        validate_attempt_scope(scope)?;
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM command_attempts WHERE view_id=?1 AND stage_id=?2 AND command_revision=?3 AND preceding_definition_revision=?4",
            params![scope.view_id.0.to_string(), scope.stage_id, scope.command_revision, scope.preceding_definition_revision],
            |row| row.get(0),
        )?;
        usize::try_from(count).map_err(|error| MemoryError::InvalidData(error.to_string()))
    }

    pub fn reserve_command_attempts(
        &mut self,
        scope: &CommandAttemptScope,
        record_ids: &[RecordId],
        capacity: usize,
    ) -> Result<CommandAttemptReservation, MemoryError> {
        validate_attempt_scope(scope)?;
        let ids = validate_attempt_ids(record_ids)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing: i64 = tx.query_row(
            "SELECT COUNT(*) FROM command_attempts WHERE view_id=?1 AND stage_id=?2 AND command_revision=?3 AND preceding_definition_revision=?4",
            params![scope.view_id.0.to_string(), scope.stage_id, scope.command_revision, scope.preceding_definition_revision],
            |row| row.get(0),
        )?;
        let existing = usize::try_from(existing)
            .map_err(|error| MemoryError::InvalidData(error.to_string()))?;
        for id in &ids {
            let attempted: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM command_attempts WHERE view_id=?1 AND stage_id=?2 AND command_revision=?3 AND preceding_definition_revision=?4 AND source_id=?5 AND sequence=?6)",
                params![scope.view_id.0.to_string(), scope.stage_id, scope.command_revision, scope.preceding_definition_revision, id.source_id.0.to_string(), id.sequence.to_string()],
                |row| row.get(0),
            )?;
            if attempted {
                return Err(MemoryError::AlreadyAttempted(*id));
            }
        }
        if existing.saturating_add(ids.len()) > capacity {
            return Err(MemoryError::AttemptCapacity {
                existing,
                requested: ids.len(),
                capacity,
            });
        }
        let token = Uuid::new_v4();
        tx.execute(
            "INSERT INTO command_attempt_batches(token,view_id,stage_id,command_revision,preceding_definition_revision,expected_count,completed) VALUES(?1,?2,?3,?4,?5,?6,0)",
            params![token.to_string(), scope.view_id.0.to_string(), scope.stage_id, scope.command_revision, scope.preceding_definition_revision, ids.len() as i64],
        )?;
        for id in &ids {
            tx.execute(
                "INSERT INTO command_attempts(view_id,stage_id,command_revision,preceding_definition_revision,source_id,sequence,batch_token,state) VALUES(?1,?2,?3,?4,?5,?6,?7,'reserved')",
                params![scope.view_id.0.to_string(), scope.stage_id, scope.command_revision, scope.preceding_definition_revision, id.source_id.0.to_string(), id.sequence.to_string(), token.to_string()],
            )?;
        }
        tx.commit()?;
        Ok(CommandAttemptReservation {
            token,
            scope: scope.clone(),
            record_ids: ids,
        })
    }

    pub fn complete_command_attempts(
        &mut self,
        reservation: &CommandAttemptReservation,
        outcomes: &[(RecordId, CommandAttemptOutcome)],
    ) -> Result<(), MemoryError> {
        validate_attempt_scope(&reservation.scope)?;
        let reserved = validate_attempt_ids(&reservation.record_ids)?;
        let outcome_ids =
            validate_attempt_ids(&outcomes.iter().map(|(id, _)| *id).collect::<Vec<_>>())?;
        if reserved.iter().copied().collect::<BTreeSet<_>>()
            != outcome_ids.iter().copied().collect::<BTreeSet<_>>()
        {
            return Err(MemoryError::InvalidAttemptBatch(
                "outcomes must exactly match the reserved record IDs".into(),
            ));
        }
        let mut encoded = Vec::with_capacity(outcomes.len());
        let mut total_bytes = 0usize;
        for (id, outcome) in outcomes {
            let item = encode_attempt_outcome(*id, outcome)?;
            total_bytes = total_bytes.saturating_add(item.payload_bytes);
            if total_bytes > MAX_COMMAND_ATTEMPT_BATCH_BYTES {
                return Err(MemoryError::InvalidAttemptBatch(format!(
                    "completion payload exceeds {MAX_COMMAND_ATTEMPT_BATCH_BYTES} bytes"
                )));
            }
            encoded.push(item);
        }

        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let batch: Option<(String, String, String, String, i64, bool)> = tx
            .query_row(
                "SELECT view_id,stage_id,command_revision,preceding_definition_revision,expected_count,completed FROM command_attempt_batches WHERE token=?1",
                [reservation.token.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
            )
            .optional()?;
        let Some((view, stage, command, preceding, count, completed)) = batch else {
            return Err(MemoryError::AttemptOwnership);
        };
        if completed
            || view != reservation.scope.view_id.0.to_string()
            || stage != reservation.scope.stage_id
            || command != reservation.scope.command_revision
            || preceding != reservation.scope.preceding_definition_revision
            || usize::try_from(count).ok() != Some(encoded.len())
        {
            return Err(MemoryError::AttemptOwnership);
        }
        for item in encoded {
            let changed = tx.execute(
                "UPDATE command_attempts SET state=?8,fields_json=?9,diagnostic=?10 WHERE view_id=?1 AND stage_id=?2 AND command_revision=?3 AND preceding_definition_revision=?4 AND source_id=?5 AND sequence=?6 AND batch_token=?7 AND state='reserved'",
                params![view, stage, command, preceding, item.id.source_id.0.to_string(), item.id.sequence.to_string(), reservation.token.to_string(), item.state, item.fields, item.diagnostic],
            )?;
            if changed != 1 {
                return Err(MemoryError::AttemptOwnership);
            }
        }
        let changed = tx.execute(
            "UPDATE command_attempt_batches SET completed=1 WHERE token=?1 AND completed=0",
            [reservation.token.to_string()],
        )?;
        if changed != 1 {
            return Err(MemoryError::AttemptOwnership);
        }
        tx.commit()?;
        Ok(())
    }

    pub fn command_attempts(
        &self,
        scope: &CommandAttemptScope,
        record_ids: &[RecordId],
    ) -> Result<Vec<CommandAttemptRecord>, MemoryError> {
        validate_attempt_scope(scope)?;
        let ids = validate_attempt_ids(record_ids)?;
        let mut records = Vec::with_capacity(ids.len());
        let mut total_bytes = 0usize;
        for id in ids {
            let stored: Option<(String, Option<Vec<u8>>, Option<String>)> = self
                .conn
                .query_row(
                    "SELECT state,fields_json,diagnostic FROM command_attempts WHERE view_id=?1 AND stage_id=?2 AND command_revision=?3 AND preceding_definition_revision=?4 AND source_id=?5 AND sequence=?6",
                    params![scope.view_id.0.to_string(), scope.stage_id, scope.command_revision, scope.preceding_definition_revision, id.source_id.0.to_string(), id.sequence.to_string()],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;
            let stored_bytes = stored.as_ref().map_or(0, |(_, fields, diagnostic)| {
                fields.as_ref().map_or(0, Vec::len) + diagnostic.as_ref().map_or(0, String::len)
            });
            total_bytes = total_bytes.checked_add(stored_bytes).ok_or_else(|| {
                MemoryError::InvalidData("command attempt read size overflow".into())
            })?;
            if total_bytes > MAX_COMMAND_ATTEMPT_BATCH_BYTES {
                return Err(MemoryError::InvalidAttemptBatch(format!(
                    "requested command attempt results exceed {MAX_COMMAND_ATTEMPT_BATCH_BYTES} bytes"
                )));
            }
            records.push(CommandAttemptRecord {
                record_id: id,
                state: decode_attempt_state(stored)?,
            });
        }
        Ok(records)
    }
}

fn acquire_legacy_recipe_lock(root: &Path) -> Result<Option<fs::File>, MemoryError> {
    match fs::symlink_metadata(root) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(MemoryError::Io {
                path: root.to_path_buf(),
                source,
            });
        }
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(MemoryError::InvalidData(
                "legacy recipe root is not a real directory".into(),
            ));
        }
        Ok(_) => {}
    }
    let path = root.join(".write.lock");
    let metadata = fs::symlink_metadata(&path).map_err(|source| MemoryError::Io {
        path: path.clone(),
        source,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(MemoryError::InvalidData(
            "legacy recipe write lock is not a regular file".into(),
        ));
    }
    let lock = fs::OpenOptions::new()
        .read(true)
        .open(&path)
        .map_err(|source| MemoryError::Io {
            path: path.clone(),
            source,
        })?;
    lock.try_lock_exclusive().map_err(|error| {
        MemoryError::InvalidData(format!("legacy recipes are being updated: {error}"))
    })?;
    Ok(Some(lock))
}

fn migrate_v1(conn: &Connection) -> Result<(), MemoryError> {
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch("CREATE TABLE sources(source_id TEXT PRIMARY KEY,definition_json BLOB NOT NULL,project TEXT,command TEXT,fields_json BLOB NOT NULL DEFAULT '{}',last_seen INTEGER NOT NULL DEFAULT 0,missing INTEGER NOT NULL DEFAULT 0); CREATE TABLE source_fingerprints(source_id TEXT NOT NULL,kind TEXT NOT NULL,value TEXT NOT NULL,PRIMARY KEY(source_id,kind,value),FOREIGN KEY(source_id) REFERENCES sources(source_id)); CREATE TABLE recipe_revisions(revision_id TEXT PRIMARY KEY,recipe_id TEXT NOT NULL,name TEXT NOT NULL,content_hash TEXT NOT NULL,document BLOB NOT NULL); CREATE TABLE recipes(recipe_id TEXT PRIMARY KEY,current_revision_id TEXT NOT NULL,name TEXT NOT NULL,source_id TEXT NOT NULL,FOREIGN KEY(current_revision_id) REFERENCES recipe_revisions(revision_id)); CREATE TABLE source_recipe_usage(source_id TEXT NOT NULL,recipe_id TEXT NOT NULL,use_count INTEGER NOT NULL,last_used INTEGER NOT NULL,PRIMARY KEY(source_id,recipe_id),FOREIGN KEY(source_id) REFERENCES sources(source_id),FOREIGN KEY(recipe_id) REFERENCES recipes(recipe_id)); CREATE TABLE suggestion_outcomes(id INTEGER PRIMARY KEY,source_id TEXT NOT NULL,recipe_id TEXT NOT NULL,revision_id TEXT NOT NULL,outcome TEXT NOT NULL CHECK(outcome IN('accepted','rejected')),recorded_at INTEGER NOT NULL); CREATE TABLE working_views(view_id TEXT PRIMARY KEY,source_id TEXT NOT NULL,name TEXT NOT NULL,applied_revision_id TEXT,applied_search TEXT NOT NULL,search_draft TEXT,applied_advanced_filter TEXT,advanced_filter_draft_json BLOB,navigation_json BLOB NOT NULL,version INTEGER NOT NULL,FOREIGN KEY(source_id) REFERENCES sources(source_id)); CREATE INDEX sources_recent_idx ON sources(last_seen DESC,source_id DESC); CREATE INDEX fingerprints_lookup_idx ON source_fingerprints(kind,value,source_id); CREATE INDEX suggestion_lookup_idx ON suggestion_outcomes(source_id,recipe_id,outcome); CREATE INDEX recipe_history_idx ON recipe_revisions(recipe_id); PRAGMA user_version=1;")?;
    tx.commit()?;
    Ok(())
}
fn migrate_v2(conn: &Connection) -> Result<(), MemoryError> {
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(
        "ALTER TABLE working_views ADD COLUMN presentation_json BLOB NOT NULL DEFAULT X'7B7D';\
         PRAGMA user_version=2;",
    )?;
    tx.commit()?;
    Ok(())
}
/// Moves bookmarks from each view to the source whose records they mark.
///
/// A bookmark marks a record, and a record belongs to a source, not to whatever
/// filter happened to be open when it was made. Every view's bookmarks are
/// adopted by the record's own source and de-duplicated by record id; when two
/// views held different notes for one record both texts are kept, joined,
/// because a note is something the user wrote and this migration may not throw
/// any of it away.
fn migrate_v6(conn: &Connection) -> Result<(), MemoryError> {
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS source_bookmarks(\
            source_id TEXT NOT NULL,\
            sequence INTEGER NOT NULL,\
            note TEXT NOT NULL,\
            PRIMARY KEY(source_id,sequence));",
    )?;
    let mut adopted: BTreeMap<SourceId, Vec<StoredBookmark>> = BTreeMap::new();
    {
        let mut statement = tx.prepare("SELECT view_id,presentation_json FROM working_views")?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        for (view_id, presentation) in rows {
            let Ok(mut presentation) = serde_json::from_slice::<PresentationState>(&presentation)
            else {
                continue;
            };
            if presentation.bookmarks.is_empty() {
                continue;
            }
            for bookmark in std::mem::take(&mut presentation.bookmarks) {
                merge_bookmark(
                    adopted.entry(bookmark.record.source_id).or_default(),
                    bookmark,
                );
            }
            tx.execute(
                "UPDATE working_views SET presentation_json=?2 WHERE view_id=?1",
                params![view_id, serde_json::to_vec(&presentation).map_err(invalid)?],
            )?;
        }
    }
    for (source_id, mut bookmarks) in adopted {
        let existing = read_source_bookmarks(&tx, std::slice::from_ref(&source_id))?;
        for bookmark in existing {
            merge_bookmark(&mut bookmarks, bookmark);
        }
        write_source_bookmarks(&tx, std::slice::from_ref(&source_id), &bookmarks)?;
    }
    tx.execute_batch("PRAGMA user_version=6;")?;
    tx.commit()?;
    Ok(())
}

/// Adds one bookmark to a source's set, keeping both notes when the record is
/// already marked with a different one.
fn merge_bookmark(bookmarks: &mut Vec<StoredBookmark>, incoming: StoredBookmark) {
    if let Some(existing) = bookmarks
        .iter_mut()
        .find(|existing| existing.record == incoming.record)
    {
        if existing.note != incoming.note && !incoming.note.is_empty() {
            if existing.note.is_empty() {
                existing.note = incoming.note;
            } else {
                let mut joined = format!("{} / {}", existing.note, incoming.note);
                let mut limit = MAX_BOOKMARK_NOTE_BYTES.min(joined.len());
                while !joined.is_char_boundary(limit) {
                    limit -= 1;
                }
                joined.truncate(limit);
                existing.note = joined;
            }
        }
        return;
    }
    if bookmarks.len() < MAX_SOURCE_BOOKMARKS {
        bookmarks.push(incoming);
    }
}

fn read_source_bookmarks(
    conn: &Connection,
    sources: &[SourceId],
) -> Result<Vec<StoredBookmark>, MemoryError> {
    let mut bookmarks = Vec::new();
    let mut statement = conn.prepare(
        "SELECT sequence,note FROM source_bookmarks WHERE source_id=?1 ORDER BY sequence",
    )?;
    for source_id in sources {
        let rows = statement
            .query_map([source_id.0.to_string()], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        for (sequence, note) in rows {
            if bookmarks.len() >= MAX_SOURCE_BOOKMARKS {
                break;
            }
            bookmarks.push(StoredBookmark {
                record: RecordId {
                    source_id: *source_id,
                    sequence: u64::try_from(sequence).unwrap_or_default(),
                },
                note,
            });
        }
    }
    Ok(bookmarks)
}

/// Replaces the bookmark set of exactly the given sources.
///
/// Only the sources a view actually contains are rewritten, so saving one view
/// can never clear a source it does not show.
fn write_source_bookmarks(
    conn: &Connection,
    sources: &[SourceId],
    bookmarks: &[StoredBookmark],
) -> Result<(), MemoryError> {
    for source_id in sources {
        conn.execute(
            "DELETE FROM source_bookmarks WHERE source_id=?1",
            [source_id.0.to_string()],
        )?;
        for bookmark in bookmarks
            .iter()
            .filter(|bookmark| bookmark.record.source_id == *source_id)
            .take(MAX_SOURCE_BOOKMARKS)
        {
            conn.execute(
                "INSERT OR REPLACE INTO source_bookmarks(source_id,sequence,note) \
                 VALUES(?1,?2,?3)",
                params![
                    source_id.0.to_string(),
                    to_i64(bookmark.record.sequence)?,
                    bookmark.note
                ],
            )?;
        }
    }
    Ok(())
}

/// The sources whose bookmarks a view shows: its ordered membership, or the
/// owning source for a view that predates ordered membership.
/// The view's presentation as it is stored: bookmarks are held by their source
/// instead, so a stale view row can never resurrect a deleted one.
fn stored_presentation(conn: &Connection, view: &WorkingView) -> Result<Vec<u8>, MemoryError> {
    let mut presentation = view.presentation.clone();
    presentation.bookmarks.clear();
    if presentation.selected_at == 0 {
        // A caller that does not know when this view was last used must not
        // erase it, the same way an ordinary save cannot change its role.
        presentation.selected_at = conn
            .query_row(
                "SELECT presentation_json FROM working_views WHERE view_id=?1",
                [view.id.0.to_string()],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()?
            .and_then(|stored| serde_json::from_slice::<PresentationState>(&stored).ok())
            .map_or(0, |stored| stored.selected_at);
    }
    serde_json::to_vec(&presentation).map_err(invalid)
}

fn view_sources(view: &WorkingView) -> Vec<SourceId> {
    if view.presentation.source_ids.is_empty() {
        vec![view.source_id]
    } else {
        view.presentation.source_ids.clone()
    }
}

/// Adds the explicit view role.
///
/// Every existing view becomes `derived`, which is the editable role it already
/// had. Nothing is promoted to `canonical` here: the canonical All events view
/// is created separately, so a view a user has filtered or renamed can never be
/// silently reinterpreted as the unfiltered one and lose its definition.
fn migrate_v5(conn: &Connection) -> Result<(), MemoryError> {
    let tx = conn.unchecked_transaction()?;
    if !column_exists(&tx, "working_views", "role")? {
        tx.execute_batch(
            "ALTER TABLE working_views ADD COLUMN role TEXT NOT NULL DEFAULT 'derived';",
        )?;
    }
    tx.execute_batch(
        "CREATE INDEX IF NOT EXISTS working_views_role_idx ON working_views(source_id,role);\
         PRAGMA user_version=5;",
    )?;
    tx.commit()?;
    Ok(())
}

/// A workspace whose `user_version` was rolled back still has the columns an
/// earlier run added, so every additive step has to be repeatable.
fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool, MemoryError> {
    let mut statement = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut names = statement.query_map([], |row| row.get::<_, String>(1))?;
    Ok(names.any(|name| name.is_ok_and(|name| name == column)))
}

fn migrate_v4(conn: &Connection) -> Result<(), MemoryError> {
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS command_attempt_batches(\
            token TEXT PRIMARY KEY,\
            view_id TEXT NOT NULL,\
            stage_id TEXT NOT NULL,\
            command_revision TEXT NOT NULL,\
            preceding_definition_revision TEXT NOT NULL,\
            expected_count INTEGER NOT NULL CHECK(expected_count BETWEEN 1 AND 1024),\
            completed INTEGER NOT NULL DEFAULT 0 CHECK(completed IN(0,1))\
         );\
         CREATE TABLE IF NOT EXISTS command_attempts(\
            view_id TEXT NOT NULL,\
            stage_id TEXT NOT NULL,\
            command_revision TEXT NOT NULL,\
            preceding_definition_revision TEXT NOT NULL,\
            source_id TEXT NOT NULL,\
            sequence TEXT NOT NULL,\
            batch_token TEXT NOT NULL,\
            state TEXT NOT NULL CHECK(state IN('reserved','ready','failed')),\
            fields_json BLOB,\
            diagnostic TEXT,\
            PRIMARY KEY(view_id,stage_id,command_revision,preceding_definition_revision,source_id,sequence),\
            FOREIGN KEY(batch_token) REFERENCES command_attempt_batches(token),\
            CHECK((state='reserved' AND fields_json IS NULL AND diagnostic IS NULL)\
               OR (state='ready' AND fields_json IS NOT NULL)\
               OR (state='failed' AND fields_json IS NULL AND diagnostic IS NOT NULL))\
         );\
         CREATE INDEX IF NOT EXISTS command_attempt_batch_idx ON command_attempts(batch_token,state);\
         PRAGMA user_version=4;",
    )?;
    tx.commit()?;
    Ok(())
}

struct EncodedAttemptOutcome {
    id: RecordId,
    state: &'static str,
    fields: Option<Vec<u8>>,
    diagnostic: Option<String>,
    payload_bytes: usize,
}

fn validate_attempt_scope(scope: &CommandAttemptScope) -> Result<(), MemoryError> {
    for (label, value) in [
        ("stage ID", scope.stage_id.as_str()),
        ("command revision", scope.command_revision.as_str()),
        (
            "preceding definition revision",
            scope.preceding_definition_revision.as_str(),
        ),
    ] {
        if value.is_empty()
            || value.len() > MAX_COMMAND_SCOPE_COMPONENT_BYTES
            || value.chars().any(char::is_control)
        {
            return Err(MemoryError::InvalidAttemptBatch(format!(
                "{label} must be 1..={MAX_COMMAND_SCOPE_COMPONENT_BYTES} bytes without controls"
            )));
        }
    }
    Ok(())
}

fn validate_attempt_ids(record_ids: &[RecordId]) -> Result<Vec<RecordId>, MemoryError> {
    if record_ids.is_empty() || record_ids.len() > MAX_COMMAND_ATTEMPT_BATCH {
        return Err(MemoryError::InvalidAttemptBatch(format!(
            "record count must be 1..={MAX_COMMAND_ATTEMPT_BATCH}"
        )));
    }
    let mut unique = BTreeSet::new();
    if record_ids.iter().any(|id| !unique.insert(*id)) {
        return Err(MemoryError::InvalidAttemptBatch(
            "duplicate record identity".into(),
        ));
    }
    Ok(record_ids.to_vec())
}

fn encode_attempt_outcome(
    id: RecordId,
    outcome: &CommandAttemptOutcome,
) -> Result<EncodedAttemptOutcome, MemoryError> {
    match outcome {
        CommandAttemptOutcome::Ready { fields, diagnostic } => {
            if fields.len() > MAX_COMMAND_ATTEMPT_FIELDS
                || fields.keys().any(|name| {
                    name.is_empty()
                        || name.len() > MAX_COMMAND_ATTEMPT_FIELD_BYTES
                        || name.chars().any(char::is_control)
                        || name == "raw"
                        || name.starts_with("_lvu_")
                })
            {
                return Err(MemoryError::InvalidAttemptBatch(format!(
                    "ready fields exceed {MAX_COMMAND_ATTEMPT_FIELDS} entries, use invalid names, or target protected/raw data"
                )));
            }
            let bytes = serde_json::to_vec(fields).map_err(invalid)?;
            if bytes.len() > MAX_COMMAND_ATTEMPT_RESULT_BYTES {
                return Err(MemoryError::InvalidAttemptBatch(format!(
                    "ready result exceeds {MAX_COMMAND_ATTEMPT_RESULT_BYTES} bytes"
                )));
            }
            if diagnostic
                .as_ref()
                .is_some_and(|value| value.len() > MAX_COMMAND_ATTEMPT_DIAGNOSTIC_BYTES)
            {
                return Err(MemoryError::InvalidAttemptBatch(format!(
                    "ready diagnostic exceeds {MAX_COMMAND_ATTEMPT_DIAGNOSTIC_BYTES} bytes"
                )));
            }
            Ok(EncodedAttemptOutcome {
                id,
                state: "ready",
                payload_bytes: bytes.len() + diagnostic.as_ref().map_or(0, String::len),
                fields: Some(bytes),
                diagnostic: diagnostic.clone(),
            })
        }
        CommandAttemptOutcome::Failed { diagnostic } => {
            if diagnostic.is_empty() || diagnostic.len() > MAX_COMMAND_ATTEMPT_DIAGNOSTIC_BYTES {
                return Err(MemoryError::InvalidAttemptBatch(format!(
                    "diagnostic must be 1..={MAX_COMMAND_ATTEMPT_DIAGNOSTIC_BYTES} bytes"
                )));
            }
            Ok(EncodedAttemptOutcome {
                id,
                state: "failed",
                payload_bytes: diagnostic.len(),
                fields: None,
                diagnostic: Some(diagnostic.clone()),
            })
        }
    }
}

fn decode_attempt_state(
    stored: Option<(String, Option<Vec<u8>>, Option<String>)>,
) -> Result<StoredCommandAttempt, MemoryError> {
    match stored {
        None => Ok(StoredCommandAttempt::NeverAttempted),
        Some((state, None, None)) if state == "reserved" => Ok(StoredCommandAttempt::Reserved),
        Some((state, Some(bytes), diagnostic)) if state == "ready" => {
            if bytes.len() > MAX_COMMAND_ATTEMPT_RESULT_BYTES
                || diagnostic
                    .as_ref()
                    .is_some_and(|value| value.len() > MAX_COMMAND_ATTEMPT_DIAGNOSTIC_BYTES)
            {
                return Err(MemoryError::InvalidData(
                    "stored command result exceeds bounds".into(),
                ));
            }
            let fields = serde_json::from_slice(&bytes).map_err(invalid)?;
            Ok(StoredCommandAttempt::Ready { fields, diagnostic })
        }
        Some((state, None, Some(diagnostic))) if state == "failed" => {
            if diagnostic.is_empty() || diagnostic.len() > MAX_COMMAND_ATTEMPT_DIAGNOSTIC_BYTES {
                return Err(MemoryError::InvalidData(
                    "stored command diagnostic exceeds bounds".into(),
                ));
            }
            Ok(StoredCommandAttempt::Failed { diagnostic })
        }
        Some(_) => Err(MemoryError::InvalidData(
            "stored command attempt has an invalid state payload".into(),
        )),
    }
}
fn import_tx(
    tx: &Transaction<'_>,
    recipe: &RecipeFile,
    _file_hash: &str,
) -> Result<(), MemoryError> {
    let document = toml::to_string_pretty(recipe)
        .map_err(|e| MemoryError::InvalidData(e.to_string()))?
        .into_bytes();
    let hash = crate::content_hash(&document);
    preflight_revision(tx, recipe)?;
    // Embedded source data is a portable snapshot. It may establish a missing
    // source identity, but cannot overwrite newer discovery/acquisition state.
    tx.execute("INSERT OR IGNORE INTO sources(source_id,definition_json,last_seen,missing) VALUES(?1,?2,0,0)",params![recipe.source.id.0.to_string(),serde_json::to_vec(&recipe.source).map_err(invalid)?])?;
    tx.execute("INSERT OR IGNORE INTO recipe_revisions(revision_id,recipe_id,name,content_hash,document) VALUES(?1,?2,?3,?4,?5)",params![recipe.revision_id.to_string(),recipe.recipe_id.0.to_string(),recipe.name,hash,document])?;
    tx.execute("INSERT INTO recipes(recipe_id,current_revision_id,name,source_id) VALUES(?1,?2,?3,?4) ON CONFLICT(recipe_id) DO UPDATE SET current_revision_id=excluded.current_revision_id,name=excluded.name",params![recipe.recipe_id.0.to_string(),recipe.revision_id.to_string(),recipe.name,recipe.source.id.0.to_string()])?;
    tx.execute("INSERT OR IGNORE INTO source_recipe_usage(source_id,recipe_id,use_count,last_used) VALUES(?1,?2,0,0)",params![recipe.source.id.0.to_string(),recipe.recipe_id.0.to_string()])?;
    Ok(())
}
/// Records one shared immutable revision without moving any current pointer.
fn import_revision_tx(tx: &Transaction<'_>, recipe: &RecipeFile) -> Result<(), MemoryError> {
    let document = toml::to_string_pretty(recipe)
        .map_err(|e| MemoryError::InvalidData(e.to_string()))?
        .into_bytes();
    let hash = crate::content_hash(&document);
    preflight_revision(tx, recipe)?;
    tx.execute("INSERT OR IGNORE INTO recipe_revisions(revision_id,recipe_id,name,content_hash,document) VALUES(?1,?2,?3,?4,?5)",params![recipe.revision_id.to_string(),recipe.recipe_id.0.to_string(),recipe.name,hash,document])?;
    Ok(())
}
fn preflight_revision(tx: &Transaction<'_>, recipe: &RecipeFile) -> Result<(), MemoryError> {
    let document = toml::to_string_pretty(recipe)
        .map_err(|e| MemoryError::InvalidData(e.to_string()))?
        .into_bytes();
    let hash = crate::content_hash(&document);
    let old: Option<(String, Vec<u8>, String)> = tx
        .query_row(
            "SELECT content_hash,document,recipe_id FROM recipe_revisions WHERE revision_id=?1",
            [recipe.revision_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    if old.as_ref().is_some_and(|value| {
        value.0 != hash || value.1 != document || value.2 != recipe.recipe_id.0.to_string()
    }) {
        return Err(MemoryError::Conflict);
    }
    Ok(())
}
fn check_limit(limit: u32) -> Result<(), MemoryError> {
    if limit == 0 || limit > MAX_PAGE {
        Err(MemoryError::InvalidLimit)
    } else {
        Ok(())
    }
}
fn validate_working_view(view: &WorkingView) -> Result<(), MemoryError> {
    let mut sources = std::collections::HashSet::new();
    if !view.presentation.source_ids.is_empty()
        && (view.presentation.source_ids.len() > 32
            || !view.presentation.source_ids.contains(&view.source_id)
            || view
                .presentation
                .source_ids
                .iter()
                .any(|id| !sources.insert(*id)))
    {
        return Err(MemoryError::InvalidData(
            "invalid ordered view sources".into(),
        ));
    }
    let mut bookmark_ids = std::collections::HashSet::new();
    if view.presentation.bookmarks.len() > 128
        || view.presentation.bookmarks.iter().any(|bookmark| {
            (bookmark.record.source_id != view.source_id
                && !view
                    .presentation
                    .source_ids
                    .contains(&bookmark.record.source_id))
                || bookmark.note.len() > 1024
                || bookmark.note.chars().any(char::is_control)
                || !bookmark_ids.insert(bookmark.record)
        })
    {
        return Err(MemoryError::InvalidData(
            "invalid bookmarks: source, duplicate identity, count or note size".into(),
        ));
    }

    if view.name.trim().is_empty() || view.name.len() > 1024 {
        return Err(MemoryError::InvalidData("invalid view name".into()));
    }
    if view.applied_search.len() > MAX_SEARCH_BYTES
        || view
            .search_draft
            .as_ref()
            .is_some_and(|value| value.len() > MAX_SEARCH_BYTES)
    {
        return Err(MemoryError::InvalidData(
            "search text exceeds 16 KiB".into(),
        ));
    }
    if let Some(expression) = &view.applied_advanced_filter
        && (expression.trim().is_empty() || expression.len() > MAX_EDITOR_BYTES)
    {
        return Err(MemoryError::InvalidData(
            "invalid applied advanced filter".into(),
        ));
    }
    if let Some(draft) = &view.advanced_filter_draft {
        let diagnostics_bytes = draft
            .diagnostics
            .iter()
            .try_fold(0usize, |total, value| total.checked_add(value.len()))
            .ok_or_else(|| MemoryError::InvalidData("draft diagnostics are too large".into()))?;
        if draft.text.len() > MAX_EDITOR_BYTES
            || draft.diagnostics.len() > MAX_DIAGNOSTICS
            || diagnostics_bytes > MAX_EDITOR_BYTES
        {
            return Err(MemoryError::InvalidData(
                "draft or diagnostics exceed bounds".into(),
            ));
        }
    }
    if view.presentation.fold_expanded.len() > MAX_FOLD_EXPANDED
        || view
            .presentation
            .fold_minimum_run
            .is_some_and(|run| !(2..=1_000_000).contains(&run))
        || view.presentation.fold_lookback > MAX_FOLD_LOOKBACK
        || view
            .presentation
            .fold_key_column
            .as_ref()
            .is_some_and(|column| column.is_empty() || column.len() > 64)
    {
        return Err(MemoryError::InvalidData(
            "invalid folding presentation".into(),
        ));
    }
    if view.presentation.pinned_columns.len() > 8
        || view
            .presentation
            .pinned_columns
            .iter()
            .any(|field| field.is_empty() || field.len() > 64)
        || view
            .presentation
            .color_field
            .as_ref()
            .is_some_and(|field| field.is_empty() || field.len() > 64)
    {
        return Err(MemoryError::InvalidData(
            "invalid presentation fields".into(),
        ));
    }
    if view
        .presentation
        .applied_enrichment
        .as_ref()
        .is_some_and(|value| value.len() > MAX_EDITOR_BYTES)
        || view
            .presentation
            .enrichment_draft
            .as_ref()
            .is_some_and(|draft| draft.text.len() > MAX_EDITOR_BYTES)
    {
        return Err(MemoryError::InvalidData("enrichment exceeds bounds".into()));
    }
    if let Some(chain) = &view.presentation.enrichment_chain {
        let mut ids = std::collections::HashSet::new();
        if chain.len() > 32
            || chain.iter().any(|stage| {
                stage.id.is_empty()
                    || stage.id.len() > 128
                    || stage.source.trim().is_empty()
                    || stage.source.len() > 16 * 1024
                    || stage.id.chars().any(char::is_control)
                    || !ids.insert(stage.id.as_str())
            })
        {
            return Err(MemoryError::InvalidData("invalid enrichment chain".into()));
        }
    }
    for target in [
        &view.presentation.enrichment_editing,
        &view.presentation.enrichment_selected,
    ]
    .into_iter()
    .flatten()
    {
        if target.is_empty() || target.len() > 128 {
            return Err(MemoryError::InvalidData(
                "invalid enrichment editor target".into(),
            ));
        }
    }
    Ok(())
}
fn to_i64(value: u64) -> Result<i64, MemoryError> {
    i64::try_from(value)
        .map_err(|_| MemoryError::InvalidData("integer exceeds SQLite range".into()))
}
fn invalid(e: serde_json::Error) -> MemoryError {
    MemoryError::InvalidData(e.to_string())
}
fn sql_invalid(e: serde_json::Error) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Blob, Box::new(e))
}
fn parse_uuid(s: String) -> Result<Uuid, rusqlite::Error> {
    Uuid::parse_str(&s).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}
fn json_opt<T: Serialize>(v: &Option<T>) -> Result<Option<Vec<u8>>, MemoryError> {
    v.as_ref()
        .map(|v| serde_json::to_vec(v).map_err(invalid))
        .transpose()
}
fn from_json_opt<T: for<'a> Deserialize<'a>>(
    v: Option<Vec<u8>>,
) -> Result<Option<T>, rusqlite::Error> {
    v.map(|v| serde_json::from_slice(&v).map_err(sql_invalid))
        .transpose()
}
fn source_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<SourceMetadata> {
    Ok(SourceMetadata {
        definition: serde_json::from_slice(&r.get::<_, Vec<u8>>(0)?).map_err(sql_invalid)?,
        project: r.get(1)?,
        command: r.get(2)?,
        fields: serde_json::from_slice(&r.get::<_, Vec<u8>>(3)?).map_err(sql_invalid)?,
        last_seen: r.get(4)?,
        missing: r.get(5)?,
    })
}

fn decode_recipe_revision(
    bytes: &[u8],
    id: RecipeId,
    revision: Uuid,
) -> Result<RecipeFile, MemoryError> {
    if bytes.len() as u64 > crate::MAX_DEFINITION_BYTES {
        return Err(RecipeError::TooLarge.into());
    }
    let text = std::str::from_utf8(bytes).map_err(|e| RecipeError::Toml(e.to_string()))?;
    let recipe: RecipeFile = toml::from_str(text).map_err(|e| RecipeError::Toml(e.to_string()))?;
    recipe.validate()?;
    if recipe.recipe_id != id || recipe.revision_id != revision {
        return Err(MemoryError::Conflict);
    }
    Ok(recipe)
}
