use std::{ffi::OsStr, io::Read, path::Path};

pub(crate) fn excluded_artifact(path: &Path) -> bool {
    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase();
    let database = [".db", ".sqlite", ".sqlite3"]
        .iter()
        .any(|suffix| name.ends_with(suffix))
        || ["-wal", "-shm", "-journal"]
            .iter()
            .any(|suffix| name.ends_with(suffix) && database_stem(&name, suffix));
    let coordination = name.ends_with(".lock")
        || name.ends_with(".lck")
        || name.ends_with(".pid")
        || name == "runtime.lock";
    let compressed_or_binary = [
        ".gz", ".bz2", ".xz", ".zst", ".zip", ".7z", ".tar", ".parquet", ".arrow", ".bin", ".so",
        ".dylib", ".dll",
    ]
    .iter()
    .any(|suffix| name.ends_with(suffix));
    database || coordination || compressed_or_binary || lvu_owned(path, &name)
}

fn database_stem(name: &str, suffix: &str) -> bool {
    let base = name.strip_suffix(suffix).unwrap_or(name);
    [".db", ".sqlite", ".sqlite3"]
        .iter()
        .any(|extension| base.ends_with(extension))
}

fn lvu_owned(path: &Path, name: &str) -> bool {
    if name.ends_with(".rows.idx") || name.starts_with(".lvu-index-") {
        return true;
    }
    let known_name = matches!(
        name,
        "capture.journal"
            | "cursor"
            | "cursor.json"
            | "catalog"
            | "catalog.json"
            | "events.jsonl"
            | "source.json"
            | "workspace.json"
            | "settings.toml"
    );
    known_name && is_lvu_location(path)
}

pub(crate) fn is_lvu_location(path: &Path) -> bool {
    if path
        .components()
        .any(|component| component.as_os_str() == OsStr::new(".lvu-captures"))
    {
        return true;
    }
    xdg_lvu_roots().iter().any(|root| path.starts_with(root))
}

fn xdg_lvu_roots() -> Vec<std::path::PathBuf> {
    let mut roots = Vec::new();
    for variable in ["XDG_DATA_HOME", "XDG_CACHE_HOME", "XDG_CONFIG_HOME"] {
        if let Some(root) = absolute_nonempty(std::env::var_os(variable)) {
            roots.push(root.join("lvu"));
        }
    }
    if let Some(home) = absolute_nonempty(std::env::var_os("HOME")) {
        roots.push(home.join(".local/share/lvu"));
        roots.push(home.join(".cache/lvu"));
        roots.push(home.join(".config/lvu"));
    }
    roots
}

fn absolute_nonempty(value: Option<std::ffi::OsString>) -> Option<std::path::PathBuf> {
    let path = std::path::PathBuf::from(value?);
    (!path.as_os_str().is_empty() && path.is_absolute()).then_some(path)
}

pub(crate) fn positive_log_name(path: &Path) -> bool {
    if excluded_artifact(path) {
        return false;
    }
    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase();
    if matches!(name.as_str(), "log" | "logfile")
        || name.ends_with(".log")
        || name.ends_with(".out")
        || name.ends_with(".err")
    {
        return true;
    }
    let Some((_, rotation)) = name.rsplit_once(".log.") else {
        return false;
    };
    !rotation.is_empty()
        && rotation
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.'))
}

pub(crate) fn bounded_text_evidence(path: &Path, maximum: usize) -> bool {
    let maximum = maximum.min(4096);
    if maximum == 0 {
        return false;
    }
    let Ok(file) = open_probe(path) else {
        return false;
    };
    if !file.metadata().is_ok_and(|metadata| metadata.is_file()) {
        return false;
    }
    let mut bytes = Vec::with_capacity(maximum);
    if file.take(maximum as u64).read_to_end(&mut bytes).is_err()
        || bytes.is_empty()
        || bytes.contains(&0)
    {
        return false;
    }
    let textlike = bytes
        .iter()
        .filter(|byte| byte.is_ascii_graphic() || matches!(byte, b' ' | b'\t' | b'\r' | b'\n'))
        .count();
    bytes.contains(&b'\n') && textlike.saturating_mul(100) >= bytes.len().saturating_mul(85)
}

#[cfg(target_os = "linux")]
fn open_probe(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
}

#[cfg(not(target_os = "linux"))]
fn open_probe(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::File::open(path)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::{ffi::CString, os::unix::ffi::OsStrExt, time::Duration};
    use tempfile::TempDir;

    #[test]
    fn replaced_regular_target_fifo_is_rejected_without_blocking_or_reading() {
        let root = TempDir::new().unwrap();
        let path = root.path().join("activity");
        std::fs::write(&path, b"prior line\n").unwrap();
        assert!(std::fs::metadata(&path).unwrap().is_file());
        std::fs::remove_file(&path).unwrap();
        let name = CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);

        let (send, receive) = std::sync::mpsc::channel();
        std::thread::spawn(move || send.send(bounded_text_evidence(&path, 4096)).unwrap());
        assert!(!receive.recv_timeout(Duration::from_millis(250)).unwrap());
    }

    #[test]
    fn xdg_roots_require_nonempty_absolute_values() {
        assert!(absolute_nonempty(None).is_none());
        assert!(absolute_nonempty(Some("".into())).is_none());
        assert!(absolute_nonempty(Some("relative/cache".into())).is_none());
        assert_eq!(
            absolute_nonempty(Some("/tmp/cache".into())),
            Some(std::path::PathBuf::from("/tmp/cache"))
        );
    }
}
