use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
};

use lvu::{StorageCategory, StorageEntry, StorageSnapshot};
use lvu_live::{DerivedArtifactIdentity, DerivedArtifactStatus, LiveRowProvider};

// Ownership-aware deletion, retention and cache-pressure handling. These
// modules are the durable-data half of storage: `storage.rs` above them still
// serves the published Storage dialog, which only reclaims verified-unused
// derived indexes. `governance` exposes the typed API the Storage screen owner
// wires; see docs/storage.md for the contract and the escalation order.
#[allow(dead_code)]
pub(crate) mod governance;
#[allow(dead_code)]
pub(crate) mod ledger;
#[allow(dead_code)]
pub(crate) mod ownership;
#[allow(dead_code)]
pub(crate) mod pressure;
#[allow(dead_code)]
pub(crate) mod retention;

const MAX_ROOT_ENTRIES: usize = 512;
const MAX_FILES: usize = 4096;
const MAX_DIRECTORIES: usize = 1024;
const MAX_DEPTH: usize = 4;
const MAX_ERRORS: usize = 16;

pub(crate) struct StorageResult {
    pub generation: u64,
    pub snapshot: StorageSnapshot,
    pub status: String,
    pub reviewed: Vec<DerivedArtifactIdentity>,
}

pub(crate) struct StorageJob {
    cancel: Arc<AtomicBool>,
    rx: mpsc::Receiver<StorageResult>,
    task: Option<JoinHandle<()>>,
}

impl StorageJob {
    pub fn start(
        generation: u64,
        root: PathBuf,
        provider: Arc<LiveRowProvider>,
        clear: bool,
        query_bytes: u64,
        query_limit: u64,
        reviewed: Vec<DerivedArtifactIdentity>,
    ) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let (tx, rx) = mpsc::sync_channel(1);
        let task = thread::spawn(move || {
            let mut cleanup = Cleanup::default();
            if clear {
                cleanup = clear_unused(&provider, &reviewed, &worker_cancel);
            }
            if worker_cancel.load(Ordering::Acquire) {
                return;
            }
            let (mut snapshot, reviewed) = scan(&root, &provider, &worker_cancel);
            snapshot.query_index_bytes = query_bytes;
            snapshot.query_index_limit = query_limit;
            let status = if clear {
                format!(
                    "cleared {}; skipped {} busy; {}",
                    bytes(cleanup.bytes),
                    cleanup.busy,
                    cleanup.message
                )
            } else if snapshot.truncated {
                "partial scan: safety limit reached; no files changed".into()
            } else {
                "scan complete; c previews cleanup, c again confirms".into()
            };
            let _ = tx.send(StorageResult {
                generation,
                snapshot,
                status,
                reviewed,
            });
        });
        Self {
            cancel,
            rx,
            task: Some(task),
        }
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }
    pub fn poll(&self) -> Option<StorageResult> {
        self.rx.try_recv().ok()
    }
    pub fn finished(&self) -> bool {
        self.task.as_ref().is_none_or(|task| task.is_finished())
    }
    pub fn join(&mut self) {
        if let Some(task) = self.task.take() {
            let _ = task.join();
        }
    }

    pub fn settle(&mut self, timeout: std::time::Duration) -> Result<(), String> {
        self.cancel();
        let deadline = std::time::Instant::now() + timeout;
        while !self.finished() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        if !self.finished() {
            return Err("storage scan did not settle before shutdown deadline".into());
        }
        self.join();
        Ok(())
    }
}

#[derive(Default)]
struct Cleanup {
    bytes: u64,
    busy: usize,
    message: String,
}

fn clear_unused(
    provider: &LiveRowProvider,
    reviewed: &[DerivedArtifactIdentity],
    cancel: &AtomicBool,
) -> Cleanup {
    let mut result = Cleanup {
        message: "raw and durable data preserved".into(),
        ..Default::default()
    };
    for identity in reviewed {
        if cancel.load(Ordering::Acquire) {
            break;
        }
        match provider.remove_unused_derived_artifact_cancellable(identity, cancel) {
            Ok(bytes) if bytes > 0 => result.bytes = result.bytes.saturating_add(bytes),
            Ok(_) => result.busy += 1,
            Err(error) => {
                result.busy += 1;
                result.message = format!("cleanup incomplete: {error}; durable data preserved");
            }
        }
    }
    result
}

fn scan(
    root: &Path,
    provider: &LiveRowProvider,
    cancel: &AtomicBool,
) -> (StorageSnapshot, Vec<DerivedArtifactIdentity>) {
    let budget = provider.storage_budget();
    let mut out = StorageSnapshot {
        row_cache_bytes: budget.row_cache_bytes as u64,
        row_cache_limit: budget.row_cache_limit as u64,
        derived_index_limit_per_source: budget.maximum_index_bytes_per_source,
        derived_index_limit_total: budget.maximum_total_index_bytes,
        ..Default::default()
    };
    let mut files = 0usize;
    let mut directories = 1usize;
    let mut reviewed = Vec::new();
    // The provider owns the configured cache directory, which may be outside
    // the durable capture root (for example under XDG_CACHE_HOME).
    scan_derived(provider, &mut out, &mut reviewed, &mut files, cancel);
    let entries = match fs::read_dir(root) {
        Ok(value) => value,
        Err(error) => {
            push_error(&mut out, format!("capture root: {error}"));
            return (out, reviewed);
        }
    };
    for (position, entry) in entries.take(MAX_ROOT_ENTRIES + 1).enumerate() {
        if position == MAX_ROOT_ENTRIES {
            out.truncated = true;
            break;
        }
        if cancel.load(Ordering::Acquire) {
            return (out, reviewed);
        }
        let Ok(entry) = entry else {
            push_error(&mut out, "capture root entry unreadable".into());
            continue;
        };
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == "workspace" {
            let bytes = tree_bytes(&path, 0, &mut files, &mut directories, &mut out, cancel);
            add(
                &mut out,
                StorageCategory::Workspace,
                "workspace memory + recipes",
                bytes,
                0,
                "durable; preserved",
            );
        } else if name == "investigations" {
            let bytes = tree_bytes(&path, 0, &mut files, &mut directories, &mut out, cancel);
            add(
                &mut out,
                StorageCategory::Investigation,
                "investigation exports + sessions",
                bytes,
                0,
                "durable; preserved",
            );
        } else if is_uuid(&name) {
            let bytes = tree_bytes(&path, 0, &mut files, &mut directories, &mut out, cancel);
            add(
                &mut out,
                StorageCategory::Capture,
                format!("source {name}"),
                bytes,
                0,
                "raw journal/catalog/cursors; preserved",
            );
        }
        if files >= MAX_FILES {
            out.truncated = true;
            break;
        }
    }
    (out, reviewed)
}

fn scan_derived(
    provider: &LiveRowProvider,
    out: &mut StorageSnapshot,
    reviewed: &mut Vec<DerivedArtifactIdentity>,
    files: &mut usize,
    cancel: &AtomicBool,
) {
    match provider.artifact_directory_is_current() {
        Ok(true) => {}
        Ok(false) => {
            push_error(
                out,
                "derived index directory changed or is a symlink; preserved without traversal"
                    .into(),
            );
            return;
        }
        Err(error) => {
            push_error(out, format!("derived index directory: {error}"));
            return;
        }
    }
    let (entries, truncated) = match provider.derived_artifact_paths(MAX_ROOT_ENTRIES) {
        Ok(v) => v,
        Err(e) => {
            push_error(out, format!("derived indexes: {e}"));
            return;
        }
    };
    out.truncated |= truncated;
    for (path, known_bytes) in entries {
        if cancel.load(Ordering::Acquire) {
            return;
        }
        *files += 1;
        match provider.inspect_derived_artifact_cancellable(&path, cancel) {
            Ok(DerivedArtifactStatus::Unused { bytes, identity }) => {
                reviewed.push(identity);
                add(
                    out,
                    StorageCategory::Derived,
                    path.file_name().unwrap_or_default().to_string_lossy(),
                    bytes,
                    bytes,
                    "unused, recomputable",
                )
            }
            Ok(DerivedArtifactStatus::Active) => {
                add(
                    out,
                    StorageCategory::Derived,
                    path.file_name().unwrap_or_default().to_string_lossy(),
                    known_bytes,
                    0,
                    "active/locked; skipped",
                );
            }
            Ok(DerivedArtifactStatus::NotOwned) => {
                add(
                    out,
                    StorageCategory::Derived,
                    path.file_name().unwrap_or_default().to_string_lossy(),
                    known_bytes,
                    0,
                    "unrecognized or symlink; preserved",
                );
            }
            Ok(DerivedArtifactStatus::Unverified { bytes, reason }) => add(
                out,
                StorageCategory::Derived,
                path.file_name().unwrap_or_default().to_string_lossy(),
                bytes,
                0,
                reason,
            ),
            Ok(DerivedArtifactStatus::Missing) => {}
            Err(error) => push_error(out, format!("{}: {error}", path.display())),
        }
    }
}

fn tree_bytes(
    path: &Path,
    depth: usize,
    files: &mut usize,
    directories: &mut usize,
    out: &mut StorageSnapshot,
    cancel: &AtomicBool,
) -> u64 {
    if cancel.load(Ordering::Acquire)
        || depth > MAX_DEPTH
        || *files >= MAX_FILES
        || *directories >= MAX_DIRECTORIES
    {
        out.truncated = true;
        return 0;
    }
    let metadata = match fs::symlink_metadata(path) {
        Ok(v) => v,
        Err(e) => {
            push_error(out, format!("{}: {e}", path.display()));
            return 0;
        }
    };
    if metadata.file_type().is_symlink() {
        return 0;
    }
    if metadata.is_file() {
        *files += 1;
        return metadata.len();
    }
    if !metadata.is_dir() {
        return 0;
    }
    *directories += 1;
    let entries = match fs::read_dir(path) {
        Ok(v) => v,
        Err(e) => {
            push_error(out, format!("{}: {e}", path.display()));
            return 0;
        }
    };
    entries
        .take(MAX_ROOT_ENTRIES + 1)
        .enumerate()
        .map(|(index, entry)| {
            if index == MAX_ROOT_ENTRIES {
                out.truncated = true;
                return 0;
            }
            match entry {
                Ok(entry) => tree_bytes(&entry.path(), depth + 1, files, directories, out, cancel),
                Err(error) => {
                    push_error(out, error.to_string());
                    0
                }
            }
        })
        .fold(0, u64::saturating_add)
}

fn add(
    out: &mut StorageSnapshot,
    category: StorageCategory,
    label: impl Into<String>,
    bytes: u64,
    reclaimable: u64,
    status: impl Into<String>,
) {
    out.total_bytes = out.total_bytes.saturating_add(bytes);
    out.reclaimable_bytes = out.reclaimable_bytes.saturating_add(reclaimable);
    out.entries.push(StorageEntry {
        category,
        label: label.into(),
        bytes,
        reclaimable,
        status: status.into(),
    });
}
fn push_error(out: &mut StorageSnapshot, error: String) {
    if out.errors.len() < MAX_ERRORS {
        out.errors.push(error);
    } else {
        out.truncated = true;
    }
}
fn is_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, b)| {
            matches!(i, 8 | 13 | 18 | 23) && b == b'-'
                || !matches!(i, 8 | 13 | 18 | 23) && b.is_ascii_hexdigit()
        })
}
fn bytes(value: u64) -> String {
    if value < 1024 {
        format!("{value} B")
    } else {
        format!("{:.1} KiB", value as f64 / 1024.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lvu_live::LiveConfig;
    use std::{fs::OpenOptions, io::Write, os::fd::AsRawFd};
    use tempfile::tempdir;

    const UNUSED: &str = "11111111-1111-1111-1111-111111111111.rows.idx";
    const ACTIVE: &str = "22222222-2222-2222-2222-222222222222.rows.idx";

    #[tokio::test]
    async fn scan_and_cleanup_preserve_durable_unknown_symlink_and_locked_files() {
        let temp = tempdir().unwrap();
        let root = temp.path();
        let cache = tempdir().unwrap();
        let derived = cache.path().join("derived");
        let capture = root.join("33333333-3333-3333-3333-333333333333");
        fs::create_dir_all(&derived).unwrap();
        fs::create_dir_all(&capture).unwrap();
        fs::create_dir_all(root.join("workspace/recipes")).unwrap();
        fs::create_dir_all(root.join("investigations/session")).unwrap();
        fs::write(derived.join(UNUSED), b"unused-index").unwrap();
        fs::write(derived.join(ACTIVE), b"active-index").unwrap();
        fs::write(derived.join("do-not-touch.bin"), b"unknown").unwrap();
        let traversal_target = root.join("55555555-5555-5555-5555-555555555555.rows.idx");
        fs::write(&traversal_target, b"outside-derived").unwrap();
        fs::write(capture.join("journal.lvu"), b"raw-sentinel").unwrap();
        fs::write(root.join("workspace/workspace.sqlite3"), b"memory-sentinel").unwrap();
        fs::write(
            root.join("investigations/session/data.parquet"),
            b"export-sentinel",
        )
        .unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            capture.join("journal.lvu"),
            derived.join("44444444-4444-4444-4444-444444444444.rows.idx"),
        )
        .unwrap();

        let mut config = LiveConfig::new(&derived);
        config.maximum_request_rows = 4;
        let provider = LiveRowProvider::new(config).unwrap();
        let locked = OpenOptions::new()
            .read(true)
            .write(true)
            .open(derived.join(ACTIVE))
            .unwrap();
        // SAFETY: `locked` owns this valid descriptor for the duration of the test.
        assert_eq!(
            unsafe { libc::flock(locked.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );

        let (before, reviewed) = scan(root, &provider, &AtomicBool::new(false));
        assert_eq!(before.reclaimable_bytes, 0);
        assert!(
            before
                .entries
                .iter()
                .any(|entry| entry.status.contains("active/locked"))
        );
        let result = clear_unused(&provider, &reviewed, &AtomicBool::new(false));
        assert_eq!(result.bytes, 0);
        assert_eq!(fs::read(derived.join(UNUSED)).unwrap(), b"unused-index");
        assert_eq!(fs::read(derived.join(ACTIVE)).unwrap(), b"active-index");
        assert_eq!(
            fs::read(derived.join("do-not-touch.bin")).unwrap(),
            b"unknown"
        );
        assert_eq!(
            provider
                .inspect_derived_artifact(&traversal_target)
                .unwrap(),
            DerivedArtifactStatus::NotOwned
        );
        assert_eq!(fs::read(traversal_target).unwrap(), b"outside-derived");
        assert_eq!(
            fs::read(capture.join("journal.lvu")).unwrap(),
            b"raw-sentinel"
        );
        assert_eq!(
            fs::read(root.join("workspace/workspace.sqlite3")).unwrap(),
            b"memory-sentinel"
        );
        assert_eq!(
            fs::read(root.join("investigations/session/data.parquet")).unwrap(),
            b"export-sentinel"
        );
        #[cfg(unix)]
        assert!(
            fs::symlink_metadata(derived.join("44444444-4444-4444-4444-444444444444.rows.idx"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[tokio::test]
    async fn bounded_scan_marks_truncation_and_honors_cancellation() {
        let temp = tempdir().unwrap();
        let capture = temp.path().join("33333333-3333-3333-3333-333333333333");
        fs::create_dir_all(&capture).unwrap();
        for index in 0..=MAX_FILES {
            let mut file = fs::File::create(capture.join(format!("{index}"))).unwrap();
            file.write_all(b"x").unwrap();
        }
        let provider = LiveRowProvider::new(LiveConfig::new(temp.path().join("derived"))).unwrap();
        let (snapshot, _) = scan(temp.path(), &provider, &AtomicBool::new(false));
        assert!(snapshot.truncated);
        let cancelled = AtomicBool::new(true);
        assert!(
            scan(temp.path(), &provider, &cancelled)
                .0
                .entries
                .is_empty()
        );
    }
}
