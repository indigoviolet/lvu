//! Bounded, read-only discovery of local log sources.

mod docker;
mod model;
mod procfs;
mod project;
mod relevance;

pub use docker::{DockerConfig, DockerRunner};
pub use model::*;
pub use procfs::ProcConfig;
pub use project::ProjectConfig;

use std::collections::BTreeMap;

/// Runs enabled providers and merges candidates which describe the same logical source.
pub async fn discover(request: DiscoveryRequest) -> DiscoveryResult {
    let started = std::time::Instant::now();
    let deadline = started + request.limits.maximum_duration;
    let mut statuses = Vec::new();
    let mut candidates = Vec::new();

    if request.limits.maximum_candidates == 0
        || request.limits.maximum_duration.is_zero()
        || request.limits.maximum_output_bytes == 0
    {
        for provider in [
            request.procfs.as_ref().map(|_| Provider::Procfs),
            request.project.as_ref().map(|_| Provider::Project),
            request.docker.as_ref().map(|_| Provider::Docker),
        ]
        .into_iter()
        .flatten()
        {
            statuses.push(ProviderStatus {
                provider,
                state: ProviderState::Limited,
                message: "zero discovery budget".into(),
            });
        }
        return DiscoveryResult {
            candidates,
            statuses,
            cancelled: request.cancel.is_cancelled(),
            timed_out: request.limits.maximum_duration.is_zero(),
        };
    }

    // Every enabled provider gets a real bounded attempt concurrently against
    // the one global deadline. The previous sequential order let Procfs/Project
    // consume the shared 1500ms budget so Docker (last) received no attempt and
    // reported only a starved "shared deadline reached". Concurrent execution
    // preserves the global wall-time, candidate, output, process and file
    // bounds (each provider enforces the same deadline/cancel/limits; the
    // coordinator caps merged candidates) while guaranteeing Docker is
    // attempted. Results are merged deterministically in the established
    // provider order after all settle, so completion order never changes
    // dedup or status order. Cancellation settles every provider worker.
    enum Immediate {
        Status(ProviderStatus),
        Run,
    }
    let procfs_plan = match &request.procfs {
        None => None,
        Some(_) if request.limits.maximum_processes == 0 || request.limits.maximum_files == 0 => {
            Some(Immediate::Status(limited(
                Provider::Procfs,
                "zero process or file budget",
            )))
        }
        Some(_) if request.cancel.is_cancelled() => {
            Some(Immediate::Status(cancelled(Provider::Procfs)))
        }
        Some(_) => Some(Immediate::Run),
    };
    let project_plan = match &request.project {
        None => None,
        Some(_) if request.limits.maximum_files == 0 => Some(Immediate::Status(limited(
            Provider::Project,
            "zero file budget",
        ))),
        Some(_) if request.cancel.is_cancelled() => {
            Some(Immediate::Status(cancelled(Provider::Project)))
        }
        Some(_) => Some(Immediate::Run),
    };
    let docker_plan = match &request.docker {
        None => None,
        Some(_) if request.cancel.is_cancelled() => {
            Some(Immediate::Status(cancelled(Provider::Docker)))
        }
        Some(_) => Some(Immediate::Run),
    };
    let DiscoveryRequest {
        limits,
        cancel,
        docker: docker_config,
        procfs: procfs_config,
        project: project_config,
    } = request;
    let procfs_future = async {
        match (procfs_plan, procfs_config) {
            (Some(Immediate::Status(status)), _) => Some((Vec::new(), status)),
            (Some(Immediate::Run), Some(config)) => {
                Some(procfs::discover(config, &limits, &cancel, deadline).await)
            }
            _ => None,
        }
    };
    let project_future = async {
        match (project_plan, project_config) {
            (Some(Immediate::Status(status)), _) => Some((Vec::new(), status)),
            (Some(Immediate::Run), Some(config)) => {
                Some(project::discover(config, &limits, &cancel, deadline).await)
            }
            _ => None,
        }
    };
    let docker_future = async {
        match (docker_plan, docker_config) {
            (Some(Immediate::Status(status)), _) => Some((Vec::new(), status)),
            (Some(Immediate::Run), Some(config)) => {
                Some(docker::discover(config, &limits, &cancel, deadline).await)
            }
            _ => None,
        }
    };
    let (procfs_outcome, project_outcome, docker_outcome) =
        tokio::join!(procfs_future, project_future, docker_future);
    for outcome in [procfs_outcome, project_outcome, docker_outcome]
        .into_iter()
        .flatten()
    {
        let (mut found, status) = outcome;
        candidates.append(&mut found);
        statuses.push(status);
    }

    let mut merged: BTreeMap<String, DiscoveryCandidate> = BTreeMap::new();
    let mut coordinator_limited = false;
    for mut candidate in candidates {
        if cancel.is_cancelled() {
            break;
        }
        if let Some(existing) = merged.get_mut(&candidate.dedup_key) {
            existing.merge(&mut candidate);
        } else if merged.len() >= limits.maximum_candidates {
            coordinator_limited = true;
        } else {
            merged.insert(candidate.dedup_key.clone(), candidate);
        }
    }
    if coordinator_limited
        && let Some(status) = statuses
            .iter_mut()
            .rev()
            .find(|status| status.state == ProviderState::Complete)
    {
        status.state = ProviderState::Limited;
        status
            .message
            .push_str("; coordinator candidate limit reached");
    }
    let cancelled = cancel.is_cancelled();
    let timed_out = std::time::Instant::now() >= deadline;
    DiscoveryResult {
        candidates: merged.into_values().collect(),
        statuses,
        cancelled,
        timed_out,
    }
}

fn limited(provider: Provider, message: &str) -> ProviderStatus {
    ProviderStatus {
        provider,
        state: ProviderState::Limited,
        message: message.into(),
    }
}
fn cancelled(provider: Provider) -> ProviderStatus {
    ProviderStatus {
        provider,
        state: ProviderState::Cancelled,
        message: "cancelled".into(),
    }
}
