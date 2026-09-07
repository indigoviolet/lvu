use lvu_core::{Acquisition, SourceDefinition, SourceId};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use uuid::Uuid;

#[derive(Clone, Debug, Default)]
pub struct CancellationToken(Arc<AtomicBool>);
impl CancellationToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

#[derive(Clone, Debug)]
pub struct DiscoveryLimits {
    pub maximum_candidates: usize,
    pub maximum_processes: usize,
    pub maximum_files: usize,
    /// Descriptors examined per process before moving on. Without it a few
    /// long-lived processes holding hundreds of open files spend the whole
    /// `maximum_files` budget in pid order, and the scan never reaches the
    /// processes a person is actually looking for.
    pub maximum_files_per_process: usize,
    pub maximum_output_bytes: usize,
    pub maximum_duration: Duration,
}
impl Default for DiscoveryLimits {
    fn default() -> Self {
        Self {
            maximum_candidates: 256,
            maximum_processes: 2048,
            maximum_files: 4096,
            maximum_files_per_process: 64,
            maximum_output_bytes: 2 * 1024 * 1024,
            maximum_duration: Duration::from_secs(5),
        }
    }
}

#[derive(Clone, Debug)]
pub struct DiscoveryRequest {
    pub limits: DiscoveryLimits,
    pub cancel: CancellationToken,
    pub docker: Option<crate::DockerConfig>,
    pub procfs: Option<crate::ProcConfig>,
    pub project: Option<crate::ProjectConfig>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Docker,
    Procfs,
    Project,
    Recent,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Low,
    Medium,
    High,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    Available,
    Unavailable,
    Unknown,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct Evidence {
    pub provider: Provider,
    pub summary: String,
    #[serde(default)]
    pub attributes: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DiscoveryCandidate {
    pub source: SourceDefinition,
    pub display_label: String,
    pub provider: Provider,
    pub evidence: Vec<Evidence>,
    pub confidence: Confidence,
    pub identity_hints: BTreeMap<String, String>,
    pub availability: Availability,
    /// Stable, deterministic key intended for persisted accept/reject memory.
    pub fingerprint: String,
    /// Internal canonical identity used to merge provider observations.
    pub dedup_key: String,
    /// True only when the candidate wraps a caller-persisted definition.
    #[serde(skip)]
    pub(crate) authoritative: bool,
}

impl DiscoveryCandidate {
    pub(crate) fn new(
        name: String,
        acquisition: Acquisition,
        provider: Provider,
        confidence: Confidence,
        identity_hints: BTreeMap<String, String>,
        dedup_key: String,
        evidence: Evidence,
    ) -> Self {
        let fingerprint = stable_fingerprint(&dedup_key);
        let id = deterministic_source_id(&fingerprint);
        Self {
            source: SourceDefinition {
                schema_version: 1,
                id,
                name: name.clone(),
                acquisition,
                identity_hints: identity_hints.clone(),
                retention: None,
            },
            display_label: name,
            provider,
            evidence: vec![evidence],
            confidence,
            identity_hints,
            availability: Availability::Available,
            fingerprint,
            dedup_key,
            authoritative: false,
        }
    }

    pub(crate) fn merge(&mut self, other: &mut Self) {
        if other.authoritative && !self.authoritative {
            self.source = other.source.clone();
            self.display_label = other.display_label.clone();
            self.provider = other.provider.clone();
            self.authoritative = true;
        }
        self.evidence.append(&mut other.evidence);
        self.evidence.sort();
        self.evidence.dedup();
        self.confidence = self.confidence.max(other.confidence);
        self.availability = aggregate_availability(&self.availability, &other.availability);
        for (key, value) in &other.identity_hints {
            self.identity_hints
                .entry(key.clone())
                .or_insert_with(|| value.clone());
            if !self.authoritative {
                self.source
                    .identity_hints
                    .entry(key.clone())
                    .or_insert_with(|| value.clone());
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProviderState {
    Complete,
    Unsupported,
    Unavailable,
    Limited,
    Cancelled,
    TimedOut,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderStatus {
    pub provider: Provider,
    pub state: ProviderState,
    pub message: String,
}

#[derive(Clone, Debug)]
pub struct DiscoveryResult {
    pub candidates: Vec<DiscoveryCandidate>,
    pub statuses: Vec<ProviderStatus>,
    pub cancelled: bool,
    pub timed_out: bool,
}

pub(crate) fn stable_fingerprint(value: &str) -> String {
    Uuid::new_v5(&DISCOVERY_NAMESPACE, value.as_bytes()).to_string()
}

fn deterministic_source_id(fingerprint: &str) -> SourceId {
    SourceId(Uuid::parse_str(fingerprint).expect("internally generated UUID"))
}

const DISCOVERY_NAMESPACE: Uuid = Uuid::from_bytes([
    0x73, 0x42, 0x5d, 0x5e, 0x67, 0x8c, 0x4e, 0x13, 0xa9, 0x6f, 0x51, 0x19, 0x4d, 0x76, 0x75, 0x31,
]);

fn aggregate_availability(left: &Availability, right: &Availability) -> Availability {
    match (left, right) {
        (Availability::Available, _) | (_, Availability::Available) => Availability::Available,
        (Availability::Unknown, _) | (_, Availability::Unknown) => Availability::Unknown,
        _ => Availability::Unavailable,
    }
}
