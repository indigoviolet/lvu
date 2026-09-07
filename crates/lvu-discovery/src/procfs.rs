use crate::project::{canonical_or_absolute, file_identity};
use crate::relevance::{bounded_text_evidence, excluded_artifact, positive_log_name};
use crate::{
    CancellationToken, Confidence, DiscoveryCandidate, DiscoveryLimits, Evidence, Provider,
    ProviderState, ProviderStatus,
};
use lvu_core::Acquisition;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    io::Read,
    path::{Path, PathBuf},
    time::Instant,
};

/// Hard ceiling on descriptors listed for one process before ordering them.
/// Listing is cheap, but it must not become unbounded for a process that holds
/// an enormous descriptor table.
const MAXIMUM_LISTED_DESCRIPTORS: usize = 4096;

#[derive(Clone, Debug)]
pub struct ProcConfig {
    pub root: PathBuf,
}
impl Default for ProcConfig {
    fn default() -> Self {
        Self {
            root: "/proc".into(),
        }
    }
}

pub(crate) async fn discover(
    config: ProcConfig,
    limits: &DiscoveryLimits,
    cancel: &CancellationToken,
    deadline: Instant,
) -> (Vec<DiscoveryCandidate>, ProviderStatus) {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (config, limits, cancel, deadline);
        return (
            Vec::new(),
            status(
                ProviderState::Unsupported,
                "Linux /proc discovery is unsupported on this platform",
            ),
        );
    }
    #[cfg(target_os = "linux")]
    {
        let limits = limits.clone();
        let cancel_clone = cancel.clone();
        match tokio::task::spawn_blocking(move || {
            scan(&config.root, &limits, &cancel_clone, deadline)
        })
        .await
        {
            Ok(value) => value,
            Err(error) => (
                Vec::new(),
                status(
                    ProviderState::Unavailable,
                    format!("proc worker failed: {error}"),
                ),
            ),
        }
    }
}

#[cfg(target_os = "linux")]
fn scan(
    root: &Path,
    limits: &DiscoveryLimits,
    cancel: &CancellationToken,
    deadline: Instant,
) -> (Vec<DiscoveryCandidate>, ProviderStatus) {
    if !root.is_dir() {
        return (
            Vec::new(),
            status(
                ProviderState::Unavailable,
                format!("{} is unavailable", root.display()),
            ),
        );
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return (
            Vec::new(),
            status(ProviderState::Unavailable, "cannot enumerate procfs"),
        );
    };
    let enumeration_limit = limits
        .maximum_processes
        .saturating_add(limits.maximum_files)
        .saturating_add(1);
    let mut pids = Vec::new();
    let mut enumeration_limited = false;
    for (index, entry) in entries.enumerate() {
        if cancel.is_cancelled() {
            return (Vec::new(), status(ProviderState::Cancelled, "cancelled"));
        }
        if Instant::now() >= deadline {
            return (
                Vec::new(),
                status(ProviderState::TimedOut, "time limit reached"),
            );
        }
        if index >= enumeration_limit {
            enumeration_limited = true;
            break;
        }
        let Ok(entry) = entry else { continue };
        let Some(pid) = entry.file_name().to_string_lossy().parse::<u32>().ok() else {
            continue;
        };
        pids.push((pid, entry.path()));
    }
    // Newest first. A bounded scan that takes the lowest process ids spends its
    // whole budget on kernel threads and init-time daemons, which never own the
    // file a person is looking for; the work someone wants to follow was almost
    // always started recently. Process id wraparound makes this a preference,
    // not a guarantee, which is all an ordering needs to be.
    pids.sort_by_key(|(pid, _)| std::cmp::Reverse(*pid));
    let discovered_processes = pids.len();
    let limited = enumeration_limited || discovered_processes > limits.maximum_processes;
    pids.truncate(limits.maximum_processes);
    let process_count = pids.len();
    let mut found = Vec::new();
    let mut candidate_limited = false;
    let mut tee_operands_examined = 0usize;
    // Tee arguments are the strongest process evidence. Scan them before broad fd
    // enumeration so a noisy process cannot exhaust the candidate budget first.
    for (pid, dir) in &pids {
        if cancel.is_cancelled() || Instant::now() >= deadline {
            break;
        }
        let cwd = std::fs::read_link(dir.join("cwd")).ok().map(|path| {
            if path.is_absolute() {
                path
            } else {
                canonical_or_absolute(&dir.join(path))
            }
        });
        let args = split_cmdline(
            &read_bounded(
                &dir.join("cmdline"),
                limits.maximum_output_bytes.min(64 * 1024),
            )
            .unwrap_or_default(),
        );
        if is_tee(&args) {
            let ppid = read_ppid(&dir.join("stat"), limits.maximum_output_bytes.min(4096));
            for operand in tee_operands(&args) {
                tee_operands_examined += 1;
                if tee_operands_examined > limits.maximum_files {
                    candidate_limited = true;
                    break;
                }
                if cancel.is_cancelled() || Instant::now() >= deadline {
                    break;
                }
                let path = if operand.is_absolute() {
                    operand
                } else if let Some(cwd) = &cwd {
                    cwd.join(operand)
                } else {
                    continue;
                };
                if is_regular_or_missing(&path) && !excluded_artifact(&path) {
                    let candidate = file_candidate(
                        path,
                        *pid,
                        ppid,
                        "tee output argument",
                        Confidence::High,
                        BTreeMap::from([("argument".into(), "tee".into())]),
                    );
                    candidate_limited |=
                        !insert_or_merge(&mut found, candidate, limits.maximum_candidates);
                }
            }
        }
    }
    let mut examined_files = tee_operands_examined;
    let mut examined_processes = 0usize;
    let mut budget_spent = false;
    for (pid, dir) in pids {
        if cancel.is_cancelled() {
            return (found, status(ProviderState::Cancelled, "cancelled"));
        }
        if Instant::now() >= deadline {
            return (found, status(ProviderState::TimedOut, "time limit reached"));
        }
        if budget_spent {
            break;
        }
        examined_processes += 1;
        let cwd = std::fs::read_link(dir.join("cwd")).ok().map(|path| {
            if path.is_absolute() {
                path
            } else {
                canonical_or_absolute(&dir.join(path))
            }
        });
        let ppid = read_ppid(&dir.join("stat"), limits.maximum_output_bytes.min(4096));
        let fd_dir = dir.join("fd");
        let Ok(fds) = std::fs::read_dir(fd_dir) else {
            continue;
        };
        // Lowest descriptors first, so a truncated look at a process is a look
        // at its stdio redirects and earliest opens rather than an arbitrary
        // slice of its sockets. Listing costs no readlink, so collecting the
        // numbers stays cheap; the hard cap keeps even that bounded.
        let mut numbered: Vec<(u32, std::path::PathBuf)> = Vec::new();
        for fd in fds.flatten() {
            if numbered.len() >= MAXIMUM_LISTED_DESCRIPTORS {
                candidate_limited = true;
                break;
            }
            let Some(number) = fd.file_name().to_str().and_then(|v| v.parse::<u32>().ok()) else {
                continue;
            };
            numbered.push((number, fd.path()));
        }
        numbered.sort_by_key(|(number, _)| *number);
        let mut process_files = 0usize;
        for (number, fd_path) in numbered {
            examined_files += 1;
            process_files += 1;
            if examined_files > limits.maximum_files {
                // Stop looking, but keep what was found: a short list of real
                // candidates is worth more than an empty one.
                budget_spent = true;
                break;
            }
            if process_files > limits.maximum_files_per_process {
                // Move to the next process rather than spending the rest of the
                // budget here. One process with hundreds of open files must not
                // decide what the whole machine looks like.
                candidate_limited = true;
                break;
            }
            let Ok(target) = std::fs::read_link(&fd_path) else {
                continue;
            };
            if special_target(&target) || target.to_string_lossy().ends_with(" (deleted)") {
                continue;
            }
            let flags = read_fd_flags(
                &dir.join("fdinfo").join(number.to_string()),
                limits.maximum_output_bytes.min(4096),
            );
            let redirected_stdio = number == 1 || number == 2;
            if redirected_stdio && flags.is_some_and(|value| !writable_flags(value)) {
                continue;
            }
            if !redirected_stdio && !flags.is_some_and(writable_flags) {
                continue;
            }
            let resolved = if target.is_absolute() {
                canonical_or_absolute(&target)
            } else if let Some(cwd) = &cwd {
                canonical_or_absolute(&cwd.join(target))
            } else {
                continue;
            };
            // Inspect the resolved pathname, never the procfd link itself. A race
            // can make this disappear; that simply removes the weak candidate.
            let Ok(metadata) = std::fs::metadata(&resolved) else {
                continue;
            };
            if !metadata.is_file() {
                continue;
            }
            if excluded_artifact(&resolved) {
                continue;
            }
            let named_log = positive_log_name(&resolved);
            let text_evidence = !redirected_stdio
                && !named_log
                && bounded_text_evidence(&resolved, limits.maximum_output_bytes.min(4096));
            if !redirected_stdio && !named_log && !text_evidence {
                continue;
            }
            let mut attributes = BTreeMap::from([("fd".into(), number.to_string())]);
            if let Some(value) = flags {
                attributes.insert("access_flags_octal".into(), format!("{value:o}"));
            }
            if named_log {
                attributes.insert("log_admission".into(), "name".into());
            } else if text_evidence {
                attributes.insert("log_admission".into(), "bounded_text_probe".into());
            } else {
                attributes.insert("log_admission".into(), "stdio_redirect".into());
            }
            let candidate = file_candidate(
                resolved,
                pid,
                ppid,
                if redirected_stdio {
                    "regular-file stdout/stderr redirect"
                } else {
                    "writable regular file descriptor"
                },
                if redirected_stdio {
                    Confidence::High
                } else {
                    Confidence::Medium
                },
                attributes,
            );
            candidate_limited |= !insert_or_merge(&mut found, candidate, limits.maximum_candidates);
        }
    }
    let unscanned = discovered_processes.saturating_sub(examined_processes);
    // `enumeration_limited` means the directory listing itself stopped early, so
    // the machine has at least this many processes rather than exactly this many.
    let total = if enumeration_limited {
        format!("at least {discovered_processes}")
    } else {
        discovered_processes.to_string()
    };
    if limited || candidate_limited || budget_spent || found.len() >= limits.maximum_candidates {
        // Name the bound that stopped the scan. "file descriptor limit" read as
        // the system's, which sent people looking at `ulimit` on a machine with
        // a million descriptors free.
        return (
            found,
            status(
                ProviderState::Limited,
                format!(
                    "scan budget reached: examined {examined_files} file descriptors \
                     across {examined_processes} of {total} processes, \
                     {unscanned} not scanned"
                ),
            ),
        );
    }
    (
        found,
        status(
            ProviderState::Complete,
            format!("examined {process_count} processes"),
        ),
    )
}

#[cfg(target_os = "linux")]
fn insert_or_merge(
    candidates: &mut Vec<DiscoveryCandidate>,
    mut candidate: DiscoveryCandidate,
    maximum: usize,
) -> bool {
    if let Some(existing) = candidates
        .iter_mut()
        .find(|existing| existing.dedup_key == candidate.dedup_key)
    {
        existing.merge(&mut candidate);
        true
    } else if candidates.len() < maximum {
        candidates.push(candidate);
        true
    } else {
        false
    }
}

#[cfg(target_os = "linux")]
fn split_cmdline(bytes: &[u8]) -> Vec<OsString> {
    use std::os::unix::ffi::OsStringExt;
    bytes
        .split(|b| *b == 0)
        .filter(|part| !part.is_empty())
        .map(|part| OsString::from_vec(part.to_vec()))
        .collect()
}
#[cfg(target_os = "linux")]
fn is_tee(args: &[OsString]) -> bool {
    args.first()
        .and_then(|arg| Path::new(arg).file_name())
        .is_some_and(|name| name == "tee")
}
#[cfg(target_os = "linux")]
fn tee_operands(args: &[OsString]) -> Vec<PathBuf> {
    let mut operands = Vec::new();
    let mut options = true;
    for arg in args.iter().skip(1) {
        if options && arg == "--" {
            options = false;
            continue;
        }
        let text = arg.to_string_lossy();
        if options && text.starts_with('-') {
            continue;
        }
        if text != "-" {
            operands.push(PathBuf::from(arg));
        }
    }
    operands
}
#[cfg(target_os = "linux")]
fn read_ppid(path: &Path, maximum: usize) -> Option<u32> {
    let value = String::from_utf8_lossy(&read_bounded(path, maximum)?).into_owned();
    let close = value.rfind(')')?;
    value
        .get(close + 2..)?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}
#[cfg(target_os = "linux")]
fn read_fd_flags(path: &Path, maximum: usize) -> Option<u32> {
    let value = String::from_utf8_lossy(&read_bounded(path, maximum)?).into_owned();
    let text = value
        .lines()
        .find_map(|line| line.strip_prefix("flags:").map(str::trim))?;
    u32::from_str_radix(text.trim_start_matches('0'), 8)
        .ok()
        .or(text.chars().all(|c| c == '0').then_some(0))
}

#[cfg(target_os = "linux")]
fn read_bounded(path: &Path, maximum: usize) -> Option<Vec<u8>> {
    let file = std::fs::File::open(path).ok()?;
    let mut bytes = Vec::with_capacity(maximum.min(8192));
    file.take(maximum.saturating_add(1) as u64)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() <= maximum).then_some(bytes)
}
#[cfg(target_os = "linux")]
fn writable_flags(flags: u32) -> bool {
    flags & 0o3 == 0o1 || flags & 0o3 == 0o2
}
#[cfg(target_os = "linux")]
fn special_target(target: &Path) -> bool {
    let value = target.to_string_lossy();
    value.starts_with("pipe:[") || value.starts_with("socket:[") || value.starts_with("anon_inode:")
}
#[cfg(target_os = "linux")]
fn is_regular_or_missing(path: &Path) -> bool {
    std::fs::metadata(path).map(|m| m.is_file()).unwrap_or(true)
}

#[cfg(target_os = "linux")]
fn file_candidate(
    path: PathBuf,
    pid: u32,
    ppid: Option<u32>,
    summary: &str,
    confidence: Confidence,
    mut attributes: BTreeMap<String, String>,
) -> DiscoveryCandidate {
    let canonical = canonical_or_absolute(&path);
    attributes.insert("pid".into(), pid.to_string());
    if let Some(ppid) = ppid {
        attributes.insert("parent_pid".into(), ppid.to_string());
    }
    let mut hints = BTreeMap::new();
    hints.insert(
        "canonical_path".into(),
        canonical.to_string_lossy().into_owned(),
    );
    let mut candidate = DiscoveryCandidate::new(
        canonical
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        Acquisition::File {
            path: canonical.clone(),
            follow: true,
        },
        Provider::Procfs,
        confidence,
        hints,
        file_identity(&canonical),
        Evidence {
            provider: Provider::Procfs,
            summary: summary.into(),
            attributes,
        },
    );
    if !canonical.exists() {
        candidate.availability = crate::Availability::Unknown;
    }
    candidate
}

fn status(state: ProviderState, message: impl Into<String>) -> ProviderStatus {
    ProviderStatus {
        provider: Provider::Procfs,
        state,
        message: message.into(),
    }
}
