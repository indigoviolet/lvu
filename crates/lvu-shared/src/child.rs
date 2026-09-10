//! Worker child entry: turn this process into the background capture worker.
//!
//! The application binary doubles as the worker executable (see
//! [`SpawnSpec`](crate::spawn::SpawnSpec)): when argv carries
//! `--worker-child`, the process never touches the terminal UI and instead
//! runs election, serves the control socket, resumes the persisted session,
//! and exits only after the last window detaches. All paths and the argv
//! shape are pinned by `spawn`/`election`; this module is the runtime that
//! executes the contract, shared by the real application wiring and the
//! integration-test harness binary so both run identical code.
//!
//! Bounds: argument parsing allocates only the two paths; diagnostics go to
//! the bounded worker log (one rotation generation); every failure mode is
//! a distinct [`spawn::exit`](crate::spawn::exit) code, never a panic.

use std::{ffi::OsString, path::PathBuf, sync::Arc};

use lvu_core::SourceDefinition;

use crate::{
    AdmissionHook, AdmissionVerdict, WorkerConfig, WorkerService,
    election::{WorkerPaths, owner_is_live, try_take_owner},
    spawn::{CAPTURE_ROOT_ARG, SOCKET_ARG, WORKER_CHILD_FLAG},
};

/// Parsed `--worker-child` invocation. `workspace_root` always derives as
/// `<capture-root>/workspace`, mirroring the application's normal startup,
/// so the worker and a foreground window never disagree about durable state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChildArgs {
    pub capture_root: PathBuf,
    pub socket_path: PathBuf,
}

impl ChildArgs {
    pub fn workspace_root(&self) -> PathBuf {
        self.capture_root.join("workspace")
    }
}

/// Split argv (without the executable) into normal startup (`None`) or a
/// worker child invocation (`Some`). The child flag is recognized only in
/// first position: anywhere else it is user input, not a mode switch, and
/// is refused loudly rather than half-adopted.
pub fn parse_child_args(args: &[OsString]) -> Result<Option<ChildArgs>, String> {
    let Some(first) = args.first() else {
        return Ok(None);
    };
    if first != WORKER_CHILD_FLAG {
        if args.iter().any(|arg| arg == WORKER_CHILD_FLAG) {
            return Err(format!(
                "{WORKER_CHILD_FLAG} must be the first argument; refusing to run half-adopted"
            ));
        }
        return Ok(None);
    }
    let mut capture_root = None;
    let mut socket_path = None;
    let mut rest = args[1..].iter();
    while let Some(flag) = rest.next() {
        let value = rest.next().ok_or_else(|| {
            format!(
                "{} expects a value after `{}`",
                WORKER_CHILD_FLAG,
                flag.to_string_lossy()
            )
        })?;
        if flag == CAPTURE_ROOT_ARG {
            capture_root = Some(PathBuf::from(value));
        } else if flag == SOCKET_ARG {
            socket_path = Some(PathBuf::from(value));
        } else {
            return Err(format!(
                "unknown {} argument `{}`",
                WORKER_CHILD_FLAG,
                flag.to_string_lossy()
            ));
        }
    }
    match (capture_root, socket_path) {
        (Some(capture_root), Some(socket_path)) => Ok(Some(ChildArgs {
            capture_root,
            socket_path,
        })),
        _ => Err(format!(
            "{WORKER_CHILD_FLAG} requires both {CAPTURE_ROOT_ARG} <dir> and {SOCKET_ARG} <path>"
        )),
    }
}

/// Admission inside the child: requesting windows already passed
/// application-side review before sending `RequestStart`, so plain `admit`
/// never refuses. `admit_known` dedups against worker state instead of
/// acquiring the same capture twice: the identical source id re-presents
/// the live capture, and an identical acquisition *with identical retention*
/// presents it too. Anything merely similar is admitted as its own capture:
/// a retention difference must never present silently (it changes what the
/// capture keeps), and policy refinements (follow / restart / header-aware
/// presentation, mirroring the application's acquisition relation) stay
/// application-side and are follow-up work, not silent worker invention.
struct ChildAdmission;

impl AdmissionHook for ChildAdmission {
    fn admit(&self, _definition: &SourceDefinition) -> AdmissionVerdict {
        AdmissionVerdict::Admit
    }

    fn admit_known(
        &self,
        definition: &SourceDefinition,
        live: &[SourceDefinition],
    ) -> AdmissionVerdict {
        for known in live {
            if known.id == definition.id {
                return AdmissionVerdict::Present { live_id: known.id };
            }
            if known.retention == definition.retention
                && acquisition_identical(&known.acquisition, &definition.acquisition)
            {
                return AdmissionVerdict::Present { live_id: known.id };
            }
        }
        AdmissionVerdict::Admit
    }
}

/// Exact structural equality of two acquisitions via their canonical DTO
/// form. `Acquisition` serializes deterministically for a fixed value
/// (maps compare order-independently), so byte-identical wire meaning reads
/// as identical here; any policy byte that differs keeps them distinct.
fn acquisition_identical(live: &lvu_core::Acquisition, proposed: &lvu_core::Acquisition) -> bool {
    serde_json::to_value(live)
        .ok()
        .zip(serde_json::to_value(proposed).ok())
        .is_some_and(|(live, proposed)| live == proposed)
}

/// Run the child to completion on a fresh single-threaded runtime and
/// return the process exit code. For binaries without a runtime (the test
/// harness). Binaries that already run inside a runtime must call
/// [`run_child`] instead: building a second runtime inside one panics.
pub fn run_child_blocking(args: ChildArgs) -> i32 {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("worker child: cannot start async runtime: {error}");
            return crate::spawn::exit::STARTUP;
        }
    };
    runtime.block_on(run_child(args))
}

/// Validate, elect, bind, resume, serve, drain, unlink. Each phase reports
/// its own exit code; only `CLEAN` means drained viewers, stopped captures,
/// and an unlinked socket. Async so a host binary with its own runtime
/// (the application) awaits it directly instead of nesting runtimes.
pub async fn run_child(args: ChildArgs) -> i32 {
    use crate::spawn::exit;

    if !args.capture_root.is_absolute() {
        eprintln!(
            "worker child: capture root must be absolute: {}",
            args.capture_root.display()
        );
        return exit::STARTUP;
    }
    let paths = WorkerPaths::new(&args.capture_root);
    if paths.socket_path() != args.socket_path {
        eprintln!(
            "worker child: socket {} is not the election socket {} for capture root {}",
            args.socket_path.display(),
            paths.socket_path().display(),
            args.capture_root.display()
        );
        return exit::STARTUP;
    }
    if let Err(error) = paths.ensure_directories() {
        eprintln!("worker child: cannot create worker directories: {error}");
        return exit::STARTUP;
    }
    rotate_log(&paths);
    // Election first: a live incumbent means our spawn lost the race and
    // the spawner attaches instead. The guard is held for the whole child
    // lifetime; dropping it (or crashing) releases in-kernel.
    let _owner = match try_take_owner(&paths) {
        Ok(Some(guard)) => guard,
        Ok(None) => match owner_is_live(&paths) {
            Ok(true) => return exit::INCUMBENT,
            Ok(false) => {
                let reason = "election refused without a live owner";
                log_line(&paths, reason);
                eprintln!("worker child: {reason}");
                return exit::STARTUP;
            }
            Err(error) => {
                log_line(&paths, &format!("owner probe failed: {error}"));
                eprintln!("worker child: owner probe failed: {error}");
                return exit::STARTUP;
            }
        },
        Err(error) => {
            eprintln!("worker child: election I/O failed: {error}");
            return exit::STARTUP;
        }
    };
    // We won the election, so any socket file left behind is stale: a live
    // worker would hold the lock we just took.
    let _ = std::fs::remove_file(&args.socket_path);
    let listener = match tokio::net::UnixListener::bind(&args.socket_path) {
        Ok(listener) => listener,
        Err(error) => {
            let reason = format!("cannot bind {}: {error}", args.socket_path.display());
            log_line(&paths, &reason);
            eprintln!("worker child: {reason}");
            return exit::SOCKET_BIND;
        }
    };
    let config = WorkerConfig::new(
        &args.capture_root,
        &args.workspace_root(),
        &args.socket_path,
    );
    let (service, session_warning) = match WorkerService::open(config, Arc::new(ChildAdmission)) {
        Ok(opened) => opened,
        Err(error) => {
            let reason = format!("cannot open worker service: {error}");
            log_line(&paths, &reason);
            eprintln!("worker child: {reason}");
            return exit::STARTUP;
        }
    };
    if let Some(warning) = session_warning {
        log_line(&paths, &format!("session warning: {warning}"));
    }
    for (id, outcome) in service.resume_session().await {
        match outcome {
            Ok(()) => log_line(&paths, &format!("resumed capture {}", id.0)),
            Err(reason) => log_line(&paths, &format!("capture {} left stopped: {reason}", id.0)),
        }
    }
    log_line(
        &paths,
        &format!("serving session {}", service.worker_session()),
    );
    service.serve(listener).await;
    // Drained (last detach past grace): stop captures, report, unlink the
    // socket while still holding the election so no window can connect to
    // a half-stopped worker.
    for (id, outcome) in service.shutdown().await {
        match outcome {
            Ok(report) => log_line(&paths, &format!("stopped capture {}: {report}", id.0)),
            Err(reason) => log_line(&paths, &format!("stop capture {}: {reason}", id.0)),
        }
    }
    drop(_owner);
    let _ = std::fs::remove_file(&args.socket_path);
    log_line(&paths, "clean shutdown");
    exit::CLEAN
}

/// Maximum worker log size before rotation, and generations kept: one
/// current file plus one `.prev`. Diagnostics stay bounded at twice this.
const MAX_LOG_BYTES: u64 = 1024 * 1024;

/// Rotate an oversize worker log before appending: the old file becomes
/// `.prev` (replacing any previous generation) and a fresh file starts.
/// Best-effort throughout — logging must never fail startup.
fn rotate_log(paths: &WorkerPaths) {
    let path = paths.worker_log();
    let oversize = std::fs::metadata(&path)
        .map(|metadata| metadata.len() > MAX_LOG_BYTES)
        .unwrap_or(false);
    if !oversize {
        return;
    }
    let previous = path.with_extension("log.prev");
    let _ = std::fs::remove_file(&previous);
    let _ = std::fs::rename(&path, &previous);
}

/// One timestamped line on the worker log. Best-effort: a failed write is
/// silently dropped because diagnostics must never wedge the worker.
fn log_line(paths: &WorkerPaths, message: &str) {
    use std::io::Write;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths.worker_log())
    {
        let _ = writeln!(file, "{now} {message}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spawn::SpawnSpec;

    fn os(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn empty_argv_is_normal_startup() {
        assert_eq!(parse_child_args(&[]).unwrap(), None);
    }

    #[test]
    fn non_child_argv_is_normal_startup() {
        assert_eq!(
            parse_child_args(&os(&["--capture-dir", "/data"])).unwrap(),
            None
        );
    }

    #[test]
    fn spawn_spec_argv_parses_to_child_args() {
        let spec = SpawnSpec::new(
            std::path::Path::new("/usr/bin/lvu"),
            std::path::Path::new("/data/cap"),
            std::path::Path::new("/data/cap/shared-worker/control.sock"),
        );
        // Skip the executable: one parser serves both binaries.
        let parsed = parse_child_args(&spec.argv()[1..]).unwrap().unwrap();
        assert_eq!(
            parsed,
            ChildArgs {
                capture_root: PathBuf::from("/data/cap"),
                socket_path: PathBuf::from("/data/cap/shared-worker/control.sock"),
            }
        );
        assert_eq!(
            parsed.workspace_root(),
            PathBuf::from("/data/cap/workspace")
        );
    }

    #[test]
    fn misplaced_child_flag_is_refused_not_half_adopted() {
        assert!(parse_child_args(&os(&["tail.log", "--worker-child"])).is_err());
    }

    #[test]
    fn child_flag_requires_both_paths() {
        assert!(parse_child_args(&os(&["--worker-child"])).is_err());
        assert!(parse_child_args(&os(&["--worker-child", "--capture-dir", "/data"])).is_err());
        assert!(
            parse_child_args(&os(&[
                "--worker-child",
                "--capture-dir",
                "/data",
                "--frobnicate",
                "x",
                "--worker-socket",
                "/data/shared-worker/control.sock"
            ]))
            .is_err()
        );
    }

    fn file_definition(id: u128) -> SourceDefinition {
        SourceDefinition {
            schema_version: 1,
            id: lvu_core::SourceId(uuid::Uuid::from_u128(id)),
            name: format!("log-{id}"),
            acquisition: lvu_core::Acquisition::File {
                path: PathBuf::from(format!("/data/{id}.log")),
                follow: true,
            },
            identity_hints: Default::default(),
            retention: None,
        }
    }

    #[test]
    fn admission_represents_live_identities() {
        let hook = ChildAdmission;
        let live = file_definition(7);
        // Same id, changed path: the live capture wins, no second start.
        let mut moved = live.clone();
        if let lvu_core::Acquisition::File { path, .. } = &mut moved.acquisition {
            *path = PathBuf::from("/elsewhere.log");
        }
        assert_eq!(
            hook.admit_known(&moved, std::slice::from_ref(&live)),
            AdmissionVerdict::Present { live_id: live.id }
        );
        // Structurally identical acquisition under a fresh id: same capture.
        let mut same = live.clone();
        same.id = lvu_core::SourceId(uuid::Uuid::from_u128(8));
        assert_eq!(
            hook.admit_known(&same, std::slice::from_ref(&live)),
            AdmissionVerdict::Present { live_id: live.id }
        );
        // A different file is its own capture.
        let mut other = live.clone();
        other.id = lvu_core::SourceId(uuid::Uuid::from_u128(9));
        if let lvu_core::Acquisition::File { path, .. } = &mut other.acquisition {
            *path = PathBuf::from("/data/other.log");
        }
        assert_eq!(
            hook.admit_known(&other, std::slice::from_ref(&live)),
            AdmissionVerdict::Admit
        );
        // Nothing live: admit.
        assert_eq!(hook.admit_known(&other, &[]), AdmissionVerdict::Admit);
    }

    #[test]
    fn admission_retention_difference_is_not_identical() {
        let hook = ChildAdmission;
        let live = file_definition(7);
        // Same acquisition bytes but different retention: must not present
        // silently, since retention changes what the capture keeps.
        let mut changed = live.clone();
        changed.id = lvu_core::SourceId(uuid::Uuid::from_u128(10));
        changed.retention = Some(lvu_core::RetentionPolicy {
            maximum_bytes: Some(1024),
            maximum_age_seconds: None,
        });
        assert_eq!(
            hook.admit_known(&changed, std::slice::from_ref(&live)),
            AdmissionVerdict::Admit
        );
    }
}
