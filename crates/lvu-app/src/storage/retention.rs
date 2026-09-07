//! Opt-in retention policy over whole source captures.
//!
//! Retention is disabled unless the user configures a limit, and it only ever
//! removes a *whole* capture. The journal is a single append-only file whose
//! byte offsets address every record; there is no segment boundary to cut at,
//! so trimming the head of a journal would either corrupt the addressing or
//! leave readers silently reading holes. Rather than pretend, this module
//! deletes complete captures through the same ownership-aware, recorded path
//! the user's explicit deletion uses, and reports what a policy cannot free.
//!
//! Anything the policy cannot reach — an active source, a pinned capture — is
//! reported as unreclaimable instead of being deleted anyway. Finite storage
//! cannot promise unlimited lossless acquisition, so the shortfall is stated.

use std::{collections::BTreeMap, sync::atomic::AtomicBool, time::Duration};

use super::{
    ledger::{DeletionCause, DeletionLedger, format_bytes},
    ownership::{
        CaptureUnit, DeletionBlocker, DeletionOutcome, DeletionPlan, DeletionTarget, OwnershipIndex,
    },
};

pub const MAX_SOURCE_RULES: usize = 32;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RetentionRule {
    /// Delete the capture once it exceeds this many bytes on disk.
    pub maximum_bytes: Option<u64>,
    /// Delete the capture once nothing has been appended for this long.
    pub maximum_age: Option<Duration>,
}

impl RetentionRule {
    pub fn is_set(&self) -> bool {
        self.maximum_bytes.is_some() || self.maximum_age.is_some()
    }
}

/// Runtime form of the configured policy.
///
/// Constructed from plain fields so the settings module and this module stay
/// independent; the application converts once at startup and on save.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RetentionRules {
    pub enabled: bool,
    /// Applies to the total size of all captures together.
    pub global: RetentionRule,
    /// Keyed by source name or source UUID, whichever the user configured.
    pub per_source: BTreeMap<String, RetentionRule>,
}

impl RetentionRules {
    pub fn from_fields(
        enabled: bool,
        global_maximum_bytes: Option<u64>,
        global_maximum_age: Option<Duration>,
        per_source: impl IntoIterator<Item = (String, Option<u64>, Option<Duration>)>,
    ) -> Self {
        Self {
            enabled,
            global: RetentionRule {
                maximum_bytes: global_maximum_bytes,
                maximum_age: global_maximum_age,
            },
            per_source: per_source
                .into_iter()
                .take(MAX_SOURCE_RULES)
                .map(|(key, maximum_bytes, maximum_age)| {
                    (
                        key,
                        RetentionRule {
                            maximum_bytes,
                            maximum_age,
                        },
                    )
                })
                .collect(),
        }
    }

    /// A policy with no limit configured never deletes anything, which is the
    /// default the product promises.
    pub fn is_active(&self) -> bool {
        self.enabled
            && (self.global.is_set() || self.per_source.values().any(RetentionRule::is_set))
    }

    pub fn rule_for(&self, capture: &CaptureUnit) -> RetentionRule {
        self.per_source
            .get(&capture.name)
            .or_else(|| self.per_source.get(&capture.source_id.0.to_string()))
            .copied()
            .unwrap_or_default()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RefusedRetention {
    pub label: String,
    pub bytes: u64,
    pub reason: String,
}

#[derive(Clone, Debug, Default)]
pub struct RetentionAssessment {
    pub active: bool,
    pub capture_bytes: u64,
    pub global_limit_bytes: Option<u64>,
    /// Bytes above the configured global limit before anything is removed.
    pub over_by_bytes: u64,
    /// Ordered deletions, each already ownership-checked and allowed.
    pub plans: Vec<DeletionPlan>,
    /// Captures a rule selected but ownership refused.
    pub refused: Vec<RefusedRetention>,
    /// Bytes a rule wanted to free but cannot, because the data is protected.
    pub unreclaimable_bytes: u64,
    /// Bytes still above the global limit after every allowed plan runs.
    pub shortfall_bytes: u64,
    pub summary: String,
}

impl RetentionAssessment {
    pub fn would_free_bytes(&self) -> u64 {
        self.plans
            .iter()
            .fold(0, |total, plan| total.saturating_add(plan.freed_bytes()))
    }
}

/// Selects captures for deletion without touching anything.
///
/// Selection order is oldest-first by last capture activity so the newest data
/// survives longest. Active and pinned captures are never selected.
pub fn assess(
    index: &OwnershipIndex,
    rules: &RetentionRules,
    now_unix_nanos: i64,
) -> RetentionAssessment {
    let capture_bytes = index.durable_capture_bytes();
    let mut out = RetentionAssessment {
        active: rules.is_active(),
        capture_bytes,
        global_limit_bytes: rules.global.maximum_bytes,
        ..Default::default()
    };
    if !out.active {
        out.summary =
            "Retention is off. Captured data is kept until you delete it explicitly.".into();
        return out;
    }

    let mut ordered: Vec<&CaptureUnit> = index.captures.iter().collect();
    ordered.sort_by_key(|capture| {
        (
            capture.last_modified_unix_nanos.unwrap_or(0),
            capture.name.clone(),
        )
    });

    let mut selected: Vec<(&CaptureUnit, String)> = Vec::new();
    for capture in &ordered {
        let rule = rules.rule_for(capture);
        if let Some(maximum) = rule.maximum_age.or(rules.global.maximum_age) {
            let age = age_nanos(capture, now_unix_nanos);
            if age.is_some_and(|age| age > maximum.as_nanos() as i128) {
                selected.push((capture, format!("older than {}", humanize(maximum))));
                continue;
            }
        }
        if let Some(maximum) = rule.maximum_bytes
            && capture.durable_bytes > maximum
        {
            selected.push((
                capture,
                format!("larger than the {} per-source limit", format_bytes(maximum)),
            ));
        }
    }

    // Global size: oldest first until the total would fit.
    if let Some(limit) = rules.global.maximum_bytes {
        out.over_by_bytes = capture_bytes.saturating_sub(limit);
        let mut projected = capture_bytes;
        for (capture, _) in &selected {
            projected = projected.saturating_sub(capture.durable_bytes);
        }
        for capture in &ordered {
            if projected <= limit {
                break;
            }
            if selected
                .iter()
                .any(|(chosen, _)| chosen.source_id == capture.source_id)
            {
                continue;
            }
            projected = projected.saturating_sub(capture.durable_bytes);
            selected.push((
                capture,
                format!("total captures above the {} limit", format_bytes(limit)),
            ));
        }
    }

    for (capture, reason) in selected {
        let plan = index.plan_delete_capture(
            capture.source_id,
            DeletionCause::Retention,
            Some(reason.clone()),
        );
        if plan.allowed() {
            out.plans.push(plan);
        } else {
            out.unreclaimable_bytes = out
                .unreclaimable_bytes
                .saturating_add(capture.durable_bytes);
            out.refused.push(RefusedRetention {
                label: capture.label(),
                bytes: capture.durable_bytes,
                reason: plan
                    .blockers
                    .first()
                    .map(DeletionBlocker::explanation)
                    .unwrap_or_else(|| "refused".into()),
            });
        }
    }

    if let Some(limit) = rules.global.maximum_bytes {
        out.shortfall_bytes = capture_bytes
            .saturating_sub(out.would_free_bytes())
            .saturating_sub(limit);
    }
    out.summary = summarize(&out);
    out
}

fn summarize(assessment: &RetentionAssessment) -> String {
    if assessment.plans.is_empty() && assessment.refused.is_empty() {
        return format!(
            "Retention is on. {} of captures is within the configured limits; nothing to remove.",
            format_bytes(assessment.capture_bytes)
        );
    }
    let mut text = format!(
        "Retention would delete {} capture(s), freeing {}. Each removal is recorded as a visible gap.",
        assessment.plans.len(),
        format_bytes(assessment.would_free_bytes())
    );
    if !assessment.refused.is_empty() {
        text.push_str(&format!(
            " {} capture(s) holding {} are protected and will not be removed.",
            assessment.refused.len(),
            format_bytes(assessment.unreclaimable_bytes)
        ));
    }
    if assessment.shortfall_bytes > 0 {
        text.push_str(&format!(
            " Even so, {} remains above the configured limit; retention will not delete protected data to reach it.",
            format_bytes(assessment.shortfall_bytes)
        ));
    }
    text
}

/// Runs an assessment. Each plan is re-validated against `index` inside
/// `execute`, so a capture that became active or pinned since the assessment is
/// refused rather than deleted.
pub fn apply(
    index: &OwnershipIndex,
    assessment: &RetentionAssessment,
    ledger: &DeletionLedger,
    cancel: &AtomicBool,
) -> Vec<DeletionOutcome> {
    let mut outcomes = Vec::new();
    for plan in &assessment.plans {
        if cancel.load(std::sync::atomic::Ordering::Acquire) {
            break;
        }
        if !matches!(plan.target, DeletionTarget::Capture(_)) {
            continue;
        }
        outcomes.push(index.execute(plan, ledger, cancel));
    }
    outcomes
}

fn age_nanos(capture: &CaptureUnit, now_unix_nanos: i64) -> Option<i128> {
    let last = capture
        .last_modified_unix_nanos
        .or(capture.first_captured_at_unix_nanos)?;
    Some(i128::from(now_unix_nanos) - i128::from(last))
}

fn humanize(value: Duration) -> String {
    let seconds = value.as_secs();
    if seconds.is_multiple_of(86_400) {
        format!("{} day(s)", seconds / 86_400)
    } else if seconds.is_multiple_of(3_600) {
        format!("{} hour(s)", seconds / 3_600)
    } else {
        format!("{seconds} second(s)")
    }
}
