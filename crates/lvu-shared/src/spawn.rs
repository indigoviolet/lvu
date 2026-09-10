//! Worker child spawn contract: argv shape, stdio discipline, exit codes,
//! and handshake bounds. The runtime wiring (fork/exec, reaping, promotion)
//! lives in the application; this module pins the exact values both ends
//! agree on so a mismatch fails loudly instead of half-attaching.
//!
//! The worker is spawned with stdio on null from birth ("owned non-terminal
//! stdio"): it never inherits the caller's terminal, so closing any window
//! cannot block on or signal the worker through stdio. Diagnostics go to the
//! bounded worker log, readiness is the socket handshake, and failures
//! surface as distinct exit codes below.

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

/// Hidden worker entry flag. Internal acquisition infrastructure: it stays
/// out of help text and takes no user-facing arguments.
pub const WORKER_CHILD_FLAG: &str = "--worker-child";
pub const CAPTURE_ROOT_ARG: &str = "--capture-dir";
pub const SOCKET_ARG: &str = "--worker-socket";

/// Worker child exit codes. `0` is a clean shutdown after last detach;
/// anything else is already an explicit, matchable failure.
pub mod exit {
    /// Clean shutdown (drained viewers, stopped captures, flushed state).
    pub const CLEAN: i32 = 0;
    /// A live worker already holds the election: attach to it instead.
    pub const INCUMBENT: i32 = 3;
    /// The control socket could not be bound after winning election.
    pub const SOCKET_BIND: i32 = 4;
    /// Election, handshake, or startup I/O failed.
    pub const STARTUP: i32 = 5;
}

/// Everything needed to spawn (or re-spawn) a worker child for one capture
/// root. Pure data: the application turns it into a `Command`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpawnSpec {
    pub executable: PathBuf,
    pub capture_root: PathBuf,
    pub socket_path: PathBuf,
}

impl SpawnSpec {
    pub fn new(executable: &Path, capture_root: &Path, socket_path: &Path) -> Self {
        Self {
            executable: executable.to_path_buf(),
            capture_root: capture_root.to_path_buf(),
            socket_path: socket_path.to_path_buf(),
        }
    }

    /// Exact argv: `[exe, --worker-child, --capture-dir <root>,
    /// --worker-socket <sock>]`. No secrets, no user input beyond paths the
    /// caller already resolved.
    pub fn argv(&self) -> Vec<OsString> {
        vec![
            self.executable.as_os_str().to_os_string(),
            WORKER_CHILD_FLAG.into(),
            CAPTURE_ROOT_ARG.into(),
            self.capture_root.as_os_str().to_os_string(),
            SOCKET_ARG.into(),
            self.socket_path.as_os_str().to_os_string(),
        ]
    }
}

/// Detach into a new session. Call ONLY inside `Command::pre_exec`
/// (post-fork, pre-exec): `setsid` is async-signal-safe, and nothing here
/// allocates. After this the worker survives its spawner's terminal and
/// holds no pty descriptor. Non-Unix targets need their native equivalent
/// (detached-process creation) at wiring time.
///
/// # Safety
///
/// The caller must guarantee pre-exec context: no other threads running and
/// no locks held, so no state can observe the session change mid-flight.
#[cfg(unix)]
pub unsafe fn pre_exec_detach() -> std::io::Result<()> {
    // `setsid` takes no pointers and only changes session state.
    if unsafe { libc::setsid() } == -1 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_shape_is_exact_and_auditable() {
        let spec = SpawnSpec::new(
            Path::new("/usr/bin/lvu"),
            Path::new("/data/cap"),
            Path::new("/data/cap/shared-worker/control.sock"),
        );
        assert_eq!(
            spec.argv(),
            vec![
                OsString::from("/usr/bin/lvu"),
                OsString::from("--worker-child"),
                OsString::from("--capture-dir"),
                OsString::from("/data/cap"),
                OsString::from("--worker-socket"),
                OsString::from("/data/cap/shared-worker/control.sock"),
            ]
        );
    }

    #[test]
    fn exit_codes_are_distinct_and_nonzero_on_failure() {
        assert_eq!(exit::CLEAN, 0);
        let failures = [exit::INCUMBENT, exit::SOCKET_BIND, exit::STARTUP];
        assert!(failures.iter().all(|code| *code != 0));
        let mut sorted = failures.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), failures.len());
    }
}
