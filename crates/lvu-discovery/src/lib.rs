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

    if let Some(config) = request.procfs {
        if request.limits.maximum_processes == 0 || request.limits.maximum_files == 0 {
            statuses.push(limited(Provider::Procfs, "zero process or file budget"));
        } else if request.cancel.is_cancelled() {
            statuses.push(cancelled(Provider::Procfs));
        } else if std::time::Instant::now() >= deadline {
            statuses.push(timed_out(Provider::Procfs));
        } else {
            let (mut found, status) =
                procfs::discover(config, &request.limits, &request.cancel, deadline).await;
            candidates.append(&mut found);
            statuses.push(status);
        }
    }
    if let Some(config) = request.project {
        if request.limits.maximum_files == 0 {
            statuses.push(limited(Provider::Project, "zero file budget"));
        } else if request.cancel.is_cancelled() {
            statuses.push(cancelled(Provider::Project));
        } else if std::time::Instant::now() >= deadline {
            statuses.push(timed_out(Provider::Project));
        } else {
            let (mut found, status) =
                project::discover(config, &request.limits, &request.cancel, deadline).await;
            candidates.append(&mut found);
            statuses.push(status);
        }
    }
    if let Some(config) = request.docker {
        if request.cancel.is_cancelled() {
            statuses.push(cancelled(Provider::Docker));
        } else if std::time::Instant::now() >= deadline {
            statuses.push(timed_out(Provider::Docker));
        } else {
            let (mut found, status) =
                docker::discover(config, &request.limits, &request.cancel, deadline).await;
            candidates.append(&mut found);
            statuses.push(status);
        }
    }

    let mut merged: BTreeMap<String, DiscoveryCandidate> = BTreeMap::new();
    let mut coordinator_limited = false;
    for mut candidate in candidates {
        if request.cancel.is_cancelled() {
            break;
        }
        if let Some(existing) = merged.get_mut(&candidate.dedup_key) {
            existing.merge(&mut candidate);
        } else if merged.len() >= request.limits.maximum_candidates {
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
    let cancelled = request.cancel.is_cancelled();
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
fn timed_out(provider: Provider) -> ProviderStatus {
    ProviderStatus {
        provider,
        state: ProviderState::TimedOut,
        message: "shared deadline reached".into(),
    }
}
