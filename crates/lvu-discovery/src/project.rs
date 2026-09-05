use crate::relevance::{excluded_artifact, is_lvu_location, positive_log_name};
use crate::{
    Availability, CancellationToken, Confidence, DiscoveryCandidate, DiscoveryLimits, Evidence,
    Provider, ProviderState, ProviderStatus,
};
use lvu_core::Acquisition;
use std::{
    collections::{BTreeMap, VecDeque},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

#[derive(Clone, Debug)]
pub struct ProjectConfig {
    pub roots: Vec<PathBuf>,
    pub recent_sources: Vec<lvu_core::SourceDefinition>,
    pub modified_within: Duration,
    pub maximum_depth: usize,
}
impl Default for ProjectConfig {
    fn default() -> Self {
        Self {
            roots: Vec::new(),
            recent_sources: Vec::new(),
            modified_within: Duration::from_secs(14 * 86400),
            maximum_depth: 6,
        }
    }
}

pub(crate) async fn discover(
    config: ProjectConfig,
    limits: &DiscoveryLimits,
    cancel: &CancellationToken,
    deadline: Instant,
) -> (Vec<DiscoveryCandidate>, ProviderStatus) {
    let limits = limits.clone();
    let cancel_clone = cancel.clone();
    match tokio::task::spawn_blocking(move || scan(config, &limits, &cancel_clone, deadline)).await
    {
        Ok(value) => value,
        Err(error) => (
            Vec::new(),
            status(
                ProviderState::Unavailable,
                format!("project worker failed: {error}"),
            ),
        ),
    }
}

fn scan(
    config: ProjectConfig,
    limits: &DiscoveryLimits,
    cancel: &CancellationToken,
    deadline: Instant,
) -> (Vec<DiscoveryCandidate>, ProviderStatus) {
    let mut found = Vec::new();
    let mut hit_limit = false;
    let mut recent_examined = 0usize;
    for source in config.recent_sources {
        if cancel.is_cancelled() {
            return (found, status(ProviderState::Cancelled, "cancelled"));
        }
        if Instant::now() >= deadline {
            return (found, status(ProviderState::TimedOut, "time limit reached"));
        }
        recent_examined += 1;
        if recent_examined > limits.maximum_files {
            hit_limit = true;
            break;
        }
        let key = source_key(&source.acquisition, None);
        let Some(dedup_key) = key else { continue };
        let remembered_artifact = match &source.acquisition {
            Acquisition::File { path, .. } => excluded_artifact(path),
            _ => false,
        };
        let evidence = Evidence {
            provider: Provider::Recent,
            summary: "previously supplied source".into(),
            attributes: BTreeMap::from([(
                "admission".into(),
                if remembered_artifact {
                    "remembered_explicit_unusual_artifact"
                } else {
                    "remembered_explicit"
                }
                .into(),
            )]),
        };
        let mut candidate = DiscoveryCandidate::new(
            source.name.clone(),
            source.acquisition.clone(),
            Provider::Recent,
            if remembered_artifact {
                Confidence::Low
            } else {
                Confidence::High
            },
            source.identity_hints.clone(),
            dedup_key,
            evidence,
        );
        candidate.source = source;
        candidate.display_label = candidate.source.name.clone();
        candidate.identity_hints = candidate.source.identity_hints.clone();
        candidate.authoritative = true;
        candidate.availability = acquisition_availability(&candidate.source.acquisition);
        hit_limit |= !insert_or_merge(&mut found, candidate, limits.maximum_candidates);
        hit_limit |= found.len() >= limits.maximum_candidates;
    }
    let mut visited = recent_examined;
    let cutoff = SystemTime::now()
        .checked_sub(config.modified_within)
        .unwrap_or(SystemTime::UNIX_EPOCH);
    for root in config.roots {
        let project = canonical_or_absolute(&root);
        if is_lvu_location(&project) {
            continue;
        }
        let mut queue = VecDeque::from([(root, 0usize)]);
        while let Some((dir, depth)) = queue.pop_front() {
            if cancel.is_cancelled() {
                return (found, status(ProviderState::Cancelled, "cancelled"));
            }
            if Instant::now() >= deadline {
                return (found, status(ProviderState::TimedOut, "time limit reached"));
            }
            if visited >= limits.maximum_files {
                return (found, status(ProviderState::Limited, "file limit reached"));
            }
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                if cancel.is_cancelled() {
                    return (found, status(ProviderState::Cancelled, "cancelled"));
                }
                if Instant::now() >= deadline {
                    return (found, status(ProviderState::TimedOut, "time limit reached"));
                }
                visited += 1;
                if visited > limits.maximum_files {
                    hit_limit = true;
                    break;
                }
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                let path = entry.path();
                if kind.is_symlink() {
                    continue;
                }
                if kind.is_dir()
                    && depth < config.maximum_depth
                    && !ignored_dir(&entry.file_name().to_string_lossy())
                    && !is_lvu_location(&path)
                {
                    queue.push_back((path, depth + 1));
                    continue;
                }
                if !kind.is_file() || !likely_log(&path) {
                    continue;
                }
                let Ok(metadata) = entry.metadata() else {
                    continue;
                };
                if metadata
                    .modified()
                    .ok()
                    .is_some_and(|modified| modified < cutoff)
                {
                    continue;
                }
                let canonical = canonical_or_absolute(&path);
                let relative = canonical
                    .strip_prefix(&project)
                    .unwrap_or(&canonical)
                    .to_string_lossy();
                let dedup_key = file_identity(&canonical);
                let mut hints = BTreeMap::new();
                hints.insert(
                    "project_root".into(),
                    project.to_string_lossy().into_owned(),
                );
                hints.insert("relative_path".into(), relative.into_owned());
                let evidence = Evidence {
                    provider: Provider::Project,
                    summary: "recent likely log file in project".into(),
                    attributes: BTreeMap::from([(
                        "modified".into(),
                        format!("{:?}", metadata.modified().ok()),
                    )]),
                };
                let candidate = DiscoveryCandidate::new(
                    path.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                    Acquisition::File {
                        path: canonical,
                        follow: true,
                    },
                    Provider::Project,
                    Confidence::Medium,
                    hints,
                    dedup_key,
                    evidence,
                );
                hit_limit |= !insert_or_merge(&mut found, candidate, limits.maximum_candidates);
                hit_limit |= found.len() >= limits.maximum_candidates;
            }
        }
    }
    if hit_limit {
        return (found, status(ProviderState::Limited, "file limit reached"));
    }
    (
        found,
        status(
            ProviderState::Complete,
            format!("examined {visited} entries"),
        ),
    )
}

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

fn ignored_dir(name: &str) -> bool {
    matches!(
        name,
        ".git" | "node_modules" | "target" | ".venv" | "vendor" | ".lvu-captures"
    )
}
fn likely_log(path: &Path) -> bool {
    positive_log_name(path)
}
pub(crate) fn canonical_or_absolute(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| {
        if path.is_absolute() {
            path.to_owned()
        } else {
            std::env::current_dir().unwrap_or_default().join(path)
        }
    })
}
pub(crate) fn source_key(acquisition: &Acquisition, project: Option<&Path>) -> Option<String> {
    match acquisition {
        Acquisition::File { path, .. } => Some(file_identity(&canonical_or_absolute(path))),
        Acquisition::Command { .. } | Acquisition::Http { .. } => Some(match project {
            Some(path) => format!("recent:{}:{acquisition:?}", path.display()),
            None => format!("recent:{acquisition:?}"),
        }),
        Acquisition::Stdin => None,
    }
}

pub(crate) fn file_identity(path: &Path) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let mut value = String::from("file:v1:");
        for byte in path.as_os_str().as_bytes() {
            use std::fmt::Write;
            write!(&mut value, "{byte:02x}").expect("writing to String cannot fail");
        }
        value
    }
    #[cfg(not(unix))]
    {
        format!("file:v1:{}", path.to_string_lossy())
    }
}

fn acquisition_availability(acquisition: &Acquisition) -> Availability {
    match acquisition {
        Acquisition::File { path, .. } => match std::fs::metadata(path) {
            Ok(metadata) if metadata.is_file() => Availability::Available,
            Ok(_) => Availability::Unavailable,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                Availability::Unknown
            }
            Err(_) => Availability::Unavailable,
        },
        Acquisition::Command { .. } | Acquisition::Http { .. } => Availability::Unknown,
        Acquisition::Stdin => Availability::Unavailable,
    }
}
fn status(state: ProviderState, message: impl Into<String>) -> ProviderStatus {
    ProviderStatus {
        provider: Provider::Project,
        state,
        message: message.into(),
    }
}

#[allow(dead_code)]
fn _availability(_: Availability) {}
