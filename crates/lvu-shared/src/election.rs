//! Worker election and viewer refcount over record-locked files.
//!
//! Layout under a capture root (all paths derived here, nowhere else):
//!
//! - `<root>/shared-worker/owner.lock` — held with a process-owned POSIX
//!   record lock (`F_SETLK`, mirroring `lvu-core journal.rs`) by exactly one
//!   worker. The kernel releases it on crash, so a killed worker strands no
//!   election state and a replacement can always be elected.
//! - `<root>/shared-worker/control.sock` — the worker's Unix socket, valid
//!   only while the owner lock is held by a live worker. When the direct
//!   path would exceed portable `sockaddr_un` byte limits the socket is
//!   deterministically indirected under a per-user runtime directory
//!   (same election, same locks, log and capture state).
//! - `<root>/shared-worker/viewers/<pid>.lock` — one record-locked file per
//!   attached window, including the spawner's. A dead window's lock releases
//!   in-kernel, so detach needs no heartbeat and leaves no stale PIDs.
//! - `<root>/shared-worker/worker.log` — bounded worker diagnostics.
//!
//! `flock(2)` is deliberately not used: it belongs to the open file
//! description and survives `fork` into children, while record locks belong
//! to the process. See the journal ownership comment for the measured
//! failure this distinction avoids.

use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

pub const WORKER_DIR_NAME: &str = "shared-worker";
pub const OWNER_LOCK_NAME: &str = "owner.lock";
pub const SOCKET_NAME: &str = "control.sock";
pub const VIEWERS_DIR_NAME: &str = "viewers";
pub const WORKER_LOG_NAME: &str = "worker.log";

/// Longest socket path used directly under the capture root, in bytes.
/// Linux allows 107 and macOS 103 bytes of `sockaddr_un` path; 100 keeps a
/// conservative portable margin. Longer capture roots use deterministic
/// indirection (see `socket_path`) instead of failing at bind.
pub const MAX_DIRECT_SOCKET_BYTES: usize = 100;

/// Domain separator for the indirect-socket key: UUIDv5 over the canonical
/// identity root (lexical fallback before existence). A dedicated
/// namespace (not a reused one) so no other key in the tree can alias a
/// socket name.
#[cfg(unix)]
const SOCKET_KEY_NAMESPACE: uuid::Uuid = uuid::Uuid::from_bytes(*b"lvu-wsock-key\x00\x00\x00");

/// Resolved socket site: the single direct/indirect decision that
/// `socket_path` and `ensure_directories` share, so the path choice and
/// the setup choice can never disagree with each other.
#[cfg(unix)]
enum SocketSite {
    Direct(PathBuf),
    Indirect(PathBuf),
}

/// All shared-worker paths for one capture root. Construction is cheap
/// and total (no I/O); `socket_path` derivation may probe the filesystem
/// (canonicalization when the root exists, lexical fallback before it),
/// so it is deterministic but not free — construct once per pass.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkerPaths {
    directory: PathBuf,
    capture_root: PathBuf,
}

impl WorkerPaths {
    pub fn new(capture_root: &Path) -> Self {
        Self {
            directory: capture_root.join(WORKER_DIR_NAME),
            capture_root: capture_root.to_path_buf(),
        }
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn owner_lock(&self) -> PathBuf {
        self.directory.join(OWNER_LOCK_NAME)
    }

    /// The control socket, derived from ONE canonical identity root so a
    /// short symlink spelling and a long spelling of one capture can
    /// never choose different sockets while sharing one owner lock:
    /// the direct layout while the canonical direct path fits the
    /// portable byte limit, otherwise a stable short path under a
    /// per-user runtime directory keyed by the same root. Owner/viewer
    /// locks, the worker log and all capture state stay under the
    /// original capture root either way — only the socket is indirected,
    /// and deterministically: every window derives the identical string
    /// from the identical capture, independent of TMPDIR/XDG. No abstract
    /// sockets (Linux-only).
    pub fn socket_path(&self) -> PathBuf {
        #[cfg(unix)]
        {
            match self.socket_site() {
                SocketSite::Direct(path) | SocketSite::Indirect(path) => path,
            }
        }
        #[cfg(not(unix))]
        {
            self.directory.join(SOCKET_NAME)
        }
    }

    /// The single direct/indirect decision point, shared by `socket_path`
    /// and directory setup so they can never disagree: a short symlink
    /// spelling resolves to the same canonical direct path (not to the
    /// runtime dir), and only a canonical direct path beyond the byte
    /// limit indirects. Both arms derive from the canonical identity
    /// root, never the raw spelling.
    #[cfg(unix)]
    fn socket_site(&self) -> SocketSite {
        let root = canonical_identity(&self.capture_root);
        let direct = root.join(WORKER_DIR_NAME).join(SOCKET_NAME);
        if socket_byte_len(&direct) <= MAX_DIRECT_SOCKET_BYTES {
            SocketSite::Direct(direct)
        } else {
            SocketSite::Indirect(
                socket_runtime_dir().join(format!("lvu-{}.sock", socket_key(&root))),
            )
        }
    }

    pub fn viewers_dir(&self) -> PathBuf {
        self.directory.join(VIEWERS_DIR_NAME)
    }

    pub fn viewer_lock(&self, pid: u32) -> PathBuf {
        self.viewers_dir().join(format!("{pid}.lock"))
    }

    pub fn worker_log(&self) -> PathBuf {
        self.directory.join(WORKER_LOG_NAME)
    }

    /// Create the directory scaffolding (idempotent). Lock/socket files
    /// themselves are created by their owners, never here. The runtime
    /// directory is prepared exactly when the socket is indirected (same
    /// decision as `socket_path`), so a bad runtime entry can never block
    /// an unrelated direct capture, and bind never meets a missing
    /// parent.
    pub fn ensure_directories(&self) -> io::Result<()> {
        fs::create_dir_all(self.viewers_dir())?;
        #[cfg(unix)]
        if matches!(self.socket_site(), SocketSite::Indirect(_)) {
            ensure_socket_runtime()?;
        }
        Ok(())
    }
}

/// Byte length of a socket path as the OS sees it (Unix paths are
/// bytes; the `sockaddr_un` limit counts bytes, not chars).
#[cfg(unix)]
fn socket_byte_len(path: &Path) -> usize {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().len()
}

#[cfg(not(unix))]
fn socket_byte_len(path: &Path) -> usize {
    path.as_os_str().to_string_lossy().len()
}

/// Canonical identity root for socket derivation: canonicalized when
/// the path exists so symlink aliases converge on one key, lexical
/// fallback before existence (deterministic per spelling). The
/// directories may not exist yet when the first window derives the
/// path; every real flow creates scaffolding before deriving, so all
/// live spellings converge once it does.
#[cfg(unix)]
fn canonical_identity(capture_root: &Path) -> PathBuf {
    fs::canonicalize(capture_root).unwrap_or_else(|_| {
        use std::path::Component;
        let mut normalized = PathBuf::new();
        for component in capture_root.components() {
            match component {
                Component::CurDir => {}
                Component::ParentDir => {
                    normalized.pop();
                }
                other => normalized.push(other.as_os_str()),
            }
        }
        normalized
    })
}

/// Short key for a canonical identity root: UUIDv5, pure derivation
/// with no filesystem probing (the caller canonicalized already).
#[cfg(unix)]
fn socket_key(canonical: &Path) -> String {
    let name: &[u8] = {
        use std::os::unix::ffi::OsStrExt;
        canonical.as_os_str().as_bytes()
    };
    uuid::Uuid::new_v5(&SOCKET_KEY_NAMESPACE, name)
        .simple()
        .to_string()
}

/// Fixed per-user runtime directory for indirected sockets, keyed by
/// EFFECTIVE uid throughout (name and ownership check agree even under
/// setuid): `/tmp/lvu-worker-<euid>`, deliberately independent of
/// TMPDIR and XDG so differing window environments cannot split one
/// capture. macOS `/tmp` being a symlink is irrelevant: both ends
/// derive the identical string, never a canonicalized one.
#[cfg(unix)]
fn socket_runtime_dir() -> PathBuf {
    PathBuf::from(format!("/tmp/lvu-worker-{}", unsafe { libc::geteuid() }))
}

/// Create the runtime directory atomically owner-only and verify it.
/// `mkdir` never follows a trailing symlink (EEXIST instead), so a
/// raced plant in world-writable /tmp cannot redirect creation; the
/// post-create re-check (never following: `symlink_metadata`) refuses
/// symlinks, non-directories, and foreign-owned directories loudly
/// instead of traversing or repairing them. Only a real directory this
/// user owns is brought to 0700 — and re-verified after, since umask
/// may have stripped bits at creation. A hostile or stale entry fails
/// the attach with a clear error, never a hijacked socket. (A swap
/// between the re-check and the chmod remains theoretically possible;
/// same accepted window as XDG_RUNTIME_DIR handling everywhere.)
#[cfg(unix)]
fn ensure_socket_runtime() -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    let dir = socket_runtime_dir();
    match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    verify_socket_runtime_dir(&dir)
}

/// Verify a runtime directory never following it: symlinks,
/// non-directories, and foreign-owned directories are refused loudly;
/// only a real directory this user owns is brought to 0700, then
/// re-lstat-revalidated (type, owner, mode) without following
/// anything. Split from creation so tests can plant collisions in
/// scratch space instead of the shared runtime dir.
#[cfg(unix)]
fn verify_socket_runtime_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let meta = fs::symlink_metadata(dir)?;
    if meta.file_type().is_symlink() || !meta.file_type().is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "socket runtime {} is not a real directory; refusing to traverse",
                dir.display()
            ),
        ));
    }
    if meta.uid() != unsafe { libc::geteuid() } {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("socket runtime {} is owned by another user", dir.display()),
        ));
    }
    let mut permissions = meta.permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(dir, permissions)?;
    // Re-lstat, never follow: confirm the tighten landed on the same
    // real directory this user owns, not on something swapped in.
    let rechecked = fs::symlink_metadata(dir)?;
    if rechecked.file_type().is_symlink() || !rechecked.file_type().is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "socket runtime {} changed under verification; refusing",
                dir.display()
            ),
        ));
    }
    if rechecked.uid() != unsafe { libc::geteuid() } {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "socket runtime {} changed owner under verification",
                dir.display()
            ),
        ));
    }
    let mode = rechecked.permissions().mode() & 0o777;
    if mode != 0o700 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "socket runtime {} is 0o{mode:o}, not owner-only 0700",
                dir.display()
            ),
        ));
    }
    Ok(())
}

/// A held owner lock: exactly one worker process holds one at a time.
/// Dropping (or crashing) releases it in-kernel. The `File` is never read
/// or written; it exists so the descriptor the record lock lives on cannot
/// be closed early.
///
/// POSIX close semantics make double-holding lethal: closing *any* descriptor
/// on a path drops *this process's* record lock on it. An in-process held
/// registry (mirroring the journal's claimed paths) therefore refuses a
/// second take deterministically instead of merging locks behind our back.
pub struct OwnerGuard {
    _file: fs::File,
    path: PathBuf,
}

impl Drop for OwnerGuard {
    fn drop(&mut self) {
        held_paths()
            .lock()
            .expect("election registry poisoned")
            .remove(&self.path);
    }
}

/// Lock paths this process currently holds. See `OwnerGuard`.
fn held_paths() -> &'static Mutex<HashSet<PathBuf>> {
    static HELD: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    HELD.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Attempt the worker election: take the owner lock without blocking.
/// `Ok(Some)` means this process is now the worker; `Ok(None)` means the
/// path is held here already or a live worker holds it (attach to its
/// socket instead); `Err` is I/O failure. Callers must already have
/// created the directories. The lock is taken on the single descriptor the
/// guard keeps: opening the path again while holding would let a later
/// close drop this process's record lock (POSIX close semantics).
pub fn try_take_owner(paths: &WorkerPaths) -> io::Result<Option<OwnerGuard>> {
    let lock_path = paths.owner_lock();
    {
        let mut held = held_paths().lock().expect("election registry poisoned");
        if !held.insert(lock_path.clone()) {
            return Ok(None);
        }
    }
    // Every early return below releases the reservation first: a failed
    // open or a contended lock must never poison later attempts with a
    // phantom hold.
    let file = match open_lock_file(&lock_path) {
        Ok(file) => file,
        Err(error) => {
            held_paths()
                .lock()
                .expect("election registry poisoned")
                .remove(&lock_path);
            return Err(error);
        }
    };
    match try_record_lock(&file) {
        Ok(true) => Ok(Some(OwnerGuard {
            _file: file,
            path: lock_path,
        })),
        Ok(false) => {
            held_paths()
                .lock()
                .expect("election registry poisoned")
                .remove(&lock_path);
            Ok(None)
        }
        Err(error) => {
            held_paths()
                .lock()
                .expect("election registry poisoned")
                .remove(&lock_path);
            Err(error)
        }
    }
}

/// True when another live process holds the owner lock. A locally acquirable
/// lock means absent-or-dead owner (or a path problem, which surfaces as an
/// error from the caller that then takes it). Never called by the holder:
/// the registry answers for our own hold without opening the path, because
/// opening and closing it would drop the very lock being probed.
pub fn owner_is_live(paths: &WorkerPaths) -> io::Result<bool> {
    if held_paths()
        .lock()
        .expect("election registry poisoned")
        .contains(&paths.owner_lock())
    {
        return Ok(true);
    }
    let file = open_lock_file(&paths.owner_lock())?;
    Ok(!try_record_lock(&file)?)
}

/// Create and hold this window's viewer lock. The returned guard must stay
/// alive for the whole attachment: dropping it (or crashing) is the detach
/// signal the worker observes. A second take of the same slot in this
/// process is refused deterministically (see `OwnerGuard`).
pub fn take_viewer_lock(paths: &WorkerPaths, pid: u32) -> io::Result<ViewerGuard> {
    let lock_path = paths.viewer_lock(pid);
    {
        let mut held = held_paths().lock().expect("election registry poisoned");
        if !held.insert(lock_path.clone()) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "viewer lock is held here; refusing to share one PID slot",
            ));
        }
    }
    // As in `try_take_owner`: every error path below releases the
    // reservation, so a transient open/lock failure never wedges the slot.
    let file = match open_lock_file(&lock_path) {
        Ok(file) => file,
        Err(error) => {
            held_paths()
                .lock()
                .expect("election registry poisoned")
                .remove(&lock_path);
            return Err(error);
        }
    };
    match try_record_lock(&file) {
        Ok(true) => Ok(ViewerGuard {
            _file: file,
            path: lock_path,
        }),
        Ok(false) => {
            held_paths()
                .lock()
                .expect("election registry poisoned")
                .remove(&lock_path);
            Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "viewer lock is held elsewhere; refusing to share one PID slot",
            ))
        }
        Err(error) => {
            held_paths()
                .lock()
                .expect("election registry poisoned")
                .remove(&lock_path);
            Err(error)
        }
    }
}

/// A held viewer lock; dropping releases the slot in-kernel and in-registry.
pub struct ViewerGuard {
    _file: fs::File,
    path: PathBuf,
}

impl Drop for ViewerGuard {
    fn drop(&mut self) {
        held_paths()
            .lock()
            .expect("election registry poisoned")
            .remove(&self.path);
    }
}

fn open_lock_file(path: &Path) -> io::Result<fs::File> {
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
}

/// Failures enumerating viewer locks. Entry I/O errors propagate instead of
/// reading as absence: undercounting a live viewer could drain a worker with
/// an audience, so the caller retains conservative liveness and retries.
#[derive(Debug)]
pub enum ElectionError {
    Io(io::Error),
    /// More directory entries examined than the bounded scan allows. Live
    /// viewers are capped by admission, so anything beyond the scan budget
    /// is stale-file accumulation the caller must reap before trusting a
    /// count; failing closed beats concluding nobody is watching.
    ScanOverflow {
        examined: usize,
    },
}

impl std::fmt::Display for ElectionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ElectionError::Io(error) => write!(formatter, "viewer enumeration I/O: {error}"),
            ElectionError::ScanOverflow { examined } => write!(
                formatter,
                "viewer directory holds more than {MAX_VIEWER_SCAN_ENTRIES} entries ({examined} seen); refusing to conclude liveness"
            ),
        }
    }
}

impl std::error::Error for ElectionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ElectionError::Io(error) => Some(error),
            ElectionError::ScanOverflow { .. } => None,
        }
    }
}

impl From<io::Error> for ElectionError {
    fn from(error: io::Error) -> Self {
        ElectionError::Io(error)
    }
}

/// Entries examined per `live_viewers` call: the admission cap plus headroom
/// for stale files awaiting reaping. Bounds the open/probe/remove work of
/// one scan even if stale files pile up across crashes.
pub const MAX_VIEWER_SCAN_ENTRIES: usize = crate::MAX_VIEWERS + 64;

/// PIDs holding a live viewer lock right now. Record locks merge within one
/// process, so our own lock file always looks acquirable to us: our own PID
/// is skipped by construction (the caller proves its own attachment by
/// holding the guard from [`take_viewer_lock`]). Stale files (dead owners)
/// are removed best-effort; a removal race with a concurrently exiting
/// viewer is harmless because only lock state, never the file list, decides
/// liveness.
pub fn live_viewers(paths: &WorkerPaths) -> Result<Vec<u32>, ElectionError> {
    let ourselves = std::process::id();
    let entries = match fs::read_dir(paths.viewers_dir()) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(ElectionError::Io(error)),
    };
    scan_viewer_entries(entries, ourselves)
}

/// Scan one directory listing, collecting live PIDs and reaping stale files.
/// `ourselves` is skipped before opening (opening our own file would drop
/// our own record lock on close under POSIX close semantics). At most
/// `MAX_VIEWER_SCAN_ENTRIES` entries are examined and at most `MAX_VIEWERS`
/// live viewers reported; anything beyond fails closed via
/// [`ElectionError`] so the worker keeps conservative liveness instead of
/// concluding an empty audience.
fn scan_viewer_entries<I>(entries: I, ourselves: u32) -> Result<Vec<u32>, ElectionError>
where
    I: Iterator<Item = io::Result<fs::DirEntry>>,
{
    let mut live = Vec::new();
    let mut examined = 0usize;
    for entry in entries {
        let entry = entry?;
        examined += 1;
        if examined > MAX_VIEWER_SCAN_ENTRIES {
            return Err(ElectionError::ScanOverflow { examined });
        }
        let Some(pid) = entry
            .path()
            .file_stem()
            .and_then(|stem| stem.to_string_lossy().parse::<u32>().ok())
        else {
            continue;
        };
        if pid == ourselves {
            continue;
        }
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(false)
            .truncate(false)
            .open(entry.path())?;
        if try_record_lock(&file)? {
            // Acquirable: owner dead. Reap the stale file; a concurrent
            // exiter racing us changes nothing observable.
            let _ = fs::remove_file(entry.path());
        } else {
            if live.len() >= crate::MAX_VIEWERS {
                return Err(ElectionError::ScanOverflow { examined });
            }
            live.push(pid);
        }
    }
    live.sort_unstable();
    Ok(live)
}

/// Non-blocking process-owned write lock, mirroring the journal ownership
/// primitive: `flock` is rejected because it follows `fork` into children.
#[cfg(unix)]
fn try_record_lock(file: &fs::File) -> io::Result<bool> {
    use std::os::unix::io::AsRawFd;
    let mut lock: libc::flock = unsafe { std::mem::zeroed() };
    lock.l_type = libc::F_WRLCK as libc::c_short;
    lock.l_whence = libc::SEEK_SET as libc::c_short;
    // SAFETY: `lock` is fully initialized and the descriptor is open; this
    // performs no I/O beyond the lock attempt itself.
    let result = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETLK, &lock) };
    if result == 0 {
        Ok(true)
    } else {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::WouldBlock {
            Ok(false)
        } else {
            Err(error)
        }
    }
}

/// Record-lock election is implemented on Unix first; other platforms need
/// their native equivalent before shared capture can run there (see the
/// portability backlog). Failing closed, never falling back to `flock`.
#[cfg(not(unix))]
fn try_record_lock(_file: &fs::File) -> io::Result<bool> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "shared capture election needs a Unix record-lock port",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_derive_from_capture_root_only() {
        let paths = WorkerPaths::new(Path::new("/data/cap"));
        assert_eq!(
            paths.owner_lock(),
            PathBuf::from("/data/cap/shared-worker/owner.lock")
        );
        assert_eq!(
            paths.socket_path(),
            PathBuf::from("/data/cap/shared-worker/control.sock")
        );
        assert_eq!(
            paths.viewer_lock(42),
            PathBuf::from("/data/cap/shared-worker/viewers/42.lock")
        );
    }

    /// A >120-byte capture root (the reported failure shape) derives one
    /// short socket shared by every spelling: the direct string would
    /// exceed `sockaddr_un`, so both the plain and trailing-slash
    /// spellings converge on the identical indirect path within the
    /// portable byte limit, while locks and log stay under the original
    /// root. Different roots must never alias one socket.
    #[cfg(unix)]
    #[test]
    fn long_capture_root_derives_one_short_shared_socket() {
        let base = tempfile::tempdir().unwrap();
        let long = base.path().join("x".repeat(120));
        std::fs::create_dir_all(&long).unwrap();
        let spelled = PathBuf::from(format!("{}/", long.display()));
        let first = WorkerPaths::new(&long);
        let second = WorkerPaths::new(&spelled);
        let socket = first.socket_path();
        assert_eq!(
            socket,
            second.socket_path(),
            "two spellings of one capture must share one socket"
        );
        assert!(
            socket_byte_len(&socket) <= MAX_DIRECT_SOCKET_BYTES,
            "indirect socket must fit the portable limit: {}",
            socket.display()
        );
        let runtime = PathBuf::from(format!("/tmp/lvu-worker-{}", unsafe { libc::geteuid() }));
        assert_eq!(
            socket.parent().expect("socket parent"),
            runtime.as_path(),
            "indirect socket lives in the fixed per-user runtime dir, never TMPDIR"
        );
        assert!(
            first.owner_lock().starts_with(&long),
            "owner lock stays under the original root"
        );
        assert!(
            first.worker_log().starts_with(&long),
            "worker log stays under the original root"
        );
        assert!(
            first.viewers_dir().starts_with(&long),
            "viewer locks stay under the original root"
        );
        first.ensure_directories().unwrap();
        assert!(socket.parent().expect("runtime dir").is_dir());
        let other = base.path().join("y".repeat(120));
        std::fs::create_dir_all(&other).unwrap();
        assert_ne!(
            WorkerPaths::new(&other).socket_path(),
            socket,
            "different captures must not share a socket"
        );
    }

    /// A short symlink spelling resolves to the same canonical direct
    /// socket — never to the runtime dir — so alias and real spellings
    /// share one election socket instead of splitting it.
    #[cfg(unix)]
    #[test]
    fn short_symlink_alias_shares_canonical_direct_socket() {
        let base = tempfile::tempdir().unwrap();
        let real = base.path().join("real");
        std::fs::create_dir_all(&real).unwrap();
        let alias = base.path().join("alias");
        std::os::unix::fs::symlink(&real, &alias).unwrap();
        let direct = WorkerPaths::new(&real).socket_path();
        assert_eq!(
            direct,
            WorkerPaths::new(&alias).socket_path(),
            "alias and real spellings must share one direct socket"
        );
        assert!(
            direct.starts_with(&real),
            "short captures keep the direct layout: {}",
            direct.display()
        );
    }

    /// Long real directory plus long symlink alias: both exceed the
    /// direct limit textually, yet canonical convergence yields one
    /// identical short socket instead of two sockets contending on one
    /// owner lock.
    #[cfg(unix)]
    #[test]
    fn long_alias_and_real_share_one_indirect_socket() {
        let base = tempfile::tempdir().unwrap();
        let real = base.path().join("z".repeat(120));
        std::fs::create_dir_all(&real).unwrap();
        let alias = base.path().join("w".repeat(120));
        std::os::unix::fs::symlink(&real, &alias).unwrap();
        let first = WorkerPaths::new(&real).socket_path();
        let second = WorkerPaths::new(&alias).socket_path();
        assert_eq!(
            first, second,
            "long alias and real dir must share one indirect socket"
        );
        assert!(
            socket_byte_len(&first) <= MAX_DIRECT_SOCKET_BYTES,
            "shared socket must fit the portable limit: {}",
            first.display()
        );
    }

    /// Collision plants in scratch space (never the real per-user runtime
    /// dir): symlinks and non-directories are refused without traversal,
    /// and a lax-but-owned directory is tightened to 0700 and passes.
    #[cfg(unix)]
    #[test]
    fn runtime_verify_refuses_collisions_and_tightens_owned_dirs() {
        let base = tempfile::tempdir().unwrap();
        let link = base.path().join("runtime-link");
        std::os::unix::fs::symlink(base.path(), &link).unwrap();
        let refused = verify_socket_runtime_dir(&link).expect_err("symlink must be refused");
        assert!(
            refused.to_string().contains("refusing to traverse"),
            "symlink refusal must name itself: {refused}"
        );
        let file = base.path().join("runtime-file");
        std::fs::write(&file, b"nope").unwrap();
        assert!(
            verify_socket_runtime_dir(&file).is_err(),
            "non-directory must be refused"
        );
        let lax = base.path().join("runtime-lax");
        std::fs::create_dir_all(&lax).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&lax).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&lax, permissions).unwrap();
        }
        verify_socket_runtime_dir(&lax).expect("owned dir tightens to 0700");
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&lax).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "lax dir must be tightened");
        }
    }

    #[test]
    fn take_and_release_cycles_cleanly() {
        let root = tempfile::tempdir().unwrap();
        let paths = WorkerPaths::new(root.path());
        paths.ensure_directories().unwrap();
        assert!(!owner_is_live(&paths).unwrap());
        // A second take in this process is refused deterministically by the
        // held registry (merging POSIX locks behind our back would let a
        // later close silently drop the election); cross-process contention
        // is covered by acceptance with a separate lock holder.
        let first = try_take_owner(&paths).unwrap().expect("first take wins");
        assert!(owner_is_live(&paths).unwrap());
        assert!(try_take_owner(&paths).unwrap().is_none());
        drop(first);
        assert!(!owner_is_live(&paths).unwrap());
        let _guard = try_take_owner(&paths).unwrap().expect("re-take after drop");
    }

    #[test]
    fn stale_viewer_files_reaped_live_ones_reported() {
        let root = tempfile::tempdir().unwrap();
        let paths = WorkerPaths::new(root.path());
        paths.ensure_directories().unwrap();
        // A lock file with no live holder is stale: reported absent, removed.
        fs::write(paths.viewer_lock(424242), b"").unwrap();
        // Our own held lock is skipped by construction (same-process locks
        // merge, so it would otherwise look acquirable).
        let _ours = take_viewer_lock(&paths, std::process::id()).unwrap();
        assert_eq!(live_viewers(&paths).unwrap(), Vec::<u32>::new());
        assert!(!paths.viewer_lock(424242).exists());
    }

    #[test]
    fn enumeration_entry_error_propagates_instead_of_reading_absent() {
        let injected = io::Error::new(io::ErrorKind::Interrupted, "injected readdir fault");
        let entries = vec![Err::<fs::DirEntry, _>(injected)].into_iter();
        assert!(matches!(
            scan_viewer_entries(entries, 1),
            Err(ElectionError::Io(_))
        ));
    }

    /// Spawn a helper holding a POSIX lock from another process: same-process
    /// record locks merge, so contention is only observable cross-process.
    /// `python3` ships with the developer environment this crate's gates run
    /// under; its absence fails loudly rather than silently passing.
    fn spawn_lock_holder(path: &Path) -> KillOnDrop {
        let script = "import fcntl, sys, time; f = open(sys.argv[1], 'w'); fcntl.lockf(f.fileno(), fcntl.LOCK_EX); print('READY', flush=True); time.sleep(60)";
        let mut child = std::process::Command::new("python3")
            .arg("-c")
            .arg(script)
            .arg(path)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("python3 must exist for lock-contention fixtures");
        wait_for_output(&mut child, "READY");
        KillOnDrop(Some(child))
    }

    /// A spawned fixture that is always reaped: drop kills and waits, so a
    /// failing assertion never leaves a 60-second sleeper behind.
    struct KillOnDrop(Option<std::process::Child>);

    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            if let Some(mut child) = self.0.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    fn wait_for_output(child: &mut std::process::Child, marker: &str) {
        use std::io::Read;
        let marker = marker.as_bytes().to_vec();
        let wanted = marker.clone();
        let mut stdout = child.stdout.take().expect("piped stdout");
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut text = Vec::new();
            let mut byte = [0u8; 1];
            loop {
                match stdout.read(&mut byte) {
                    Ok(0) => break,
                    Ok(_) => {
                        text.extend_from_slice(&byte);
                        if text.ends_with(wanted.as_slice()) {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            let _ = tx.send(text);
        });
        match rx.recv_timeout(std::time::Duration::from_secs(10)) {
            Ok(text) => assert!(
                text.ends_with(marker.as_slice()),
                "lock holder exited without signalling readiness"
            ),
            Err(_) => panic!("lock holder never signalled readiness"),
        }
    }

    fn wait_until_locked(paths: &WorkerPaths) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if try_take_owner(paths).unwrap().is_none() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("foreign holder never became visible");
    }

    #[test]
    fn owner_contention_resolves_across_processes() {
        let root = tempfile::tempdir().unwrap();
        let paths = WorkerPaths::new(root.path());
        paths.ensure_directories().unwrap();
        let holder = spawn_lock_holder(&paths.owner_lock());
        // While the foreign process holds the lock: election loses and the
        // probe reports live ownership.
        wait_until_locked(&paths);
        assert!(try_take_owner(&paths).unwrap().is_none());
        assert!(owner_is_live(&paths).unwrap());
        // Dropping the holder kills and reaps it, which releases in-kernel:
        // election succeeds again with no manual cleanup.
        drop(holder);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if try_take_owner(&paths).unwrap().is_some() {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!("lock not released after holder death");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(!owner_is_live(&paths).unwrap());
    }

    #[test]
    fn viewer_liveness_reported_across_processes_then_reaped() {
        let root = tempfile::tempdir().unwrap();
        let paths = WorkerPaths::new(root.path());
        paths.ensure_directories().unwrap();
        // Hold a *viewer* slot from another process under a fixed PID name.
        // (A real PID filename is unnecessary: the scan keys on filenames,
        // and liveness comes from the foreign lock, not the number.)
        let viewer_pid = 31337u32;
        let holder = spawn_lock_holder(&paths.viewer_lock(viewer_pid));
        assert_eq!(live_viewers(&paths).unwrap(), vec![viewer_pid]);
        drop(holder);
        // Lock release on death is in-kernel but asynchronous to observe;
        // poll boundedly rather than asserting once.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let live = live_viewers(&paths).unwrap();
            if live.is_empty() && !paths.viewer_lock(viewer_pid).exists() {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!("dead viewer still reported live: {live:?}");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}
