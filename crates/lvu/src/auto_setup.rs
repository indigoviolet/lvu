//! UI-facing automatic log setup types.
//!
//! The bridge and durable receipt store deliberately do not live in this
//! crate.  This module is the narrow seam they hand validated proposals and
//! receipts through: it can describe only the existing native enrichment,
//! pin, colour-rule and grouping editors.  There is no field for a filter,
//! command, time window or source mutation.

use lvu_core::{CommandDefinition, CommandProgram, RestartPolicy};

use crate::app::{
    CaptureTimePolicy, CaptureTimeRange, ColorRule, EnrichmentDefinition, RecipeConfig, TimeBasis,
};

/// Bounded, non-modal progress shown while the raw view remains usable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AutoSetupStage {
    WaitingForRows,
    Sampling,
    Analyzing,
    Validating,
    /// A schema-checked proposal is waiting for explicit review. Nothing is
    /// applied while a proposal sits in this stage; the inspector shows the
    /// full bounded summary and only its Apply action installs the Enhanced
    /// view. Close (the safe default) preserves the pending proposal.
    Review,
    Applying,
    Unavailable,
    Applied,
}

impl AutoSetupStage {
    pub fn label(self) -> &'static str {
        match self {
            Self::WaitingForRows => "waiting for rows",
            Self::Sampling => "sampling",
            Self::Analyzing => "analyzing",
            Self::Validating => "validating",
            Self::Review => "review",
            Self::Applying => "applying",
            Self::Unavailable => "unavailable",
            Self::Applied => "applied",
        }
    }
}

/// The single lifecycle row the shell may show for automatic setup.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AutoSetupStatus {
    pub source_id: String,
    pub origin_view_id: String,
    /// Existing object first, then operation/status (the product's flow
    /// grammar): `payments · automatic setup: analyzing`.
    pub object_name: String,
    pub stage: AutoSetupStage,
    /// The local Paseo conversation once the bridge has created it. Kept
    /// separately from prose so a narrow inspector never has to parse it.
    pub session_id: Option<String>,
    pub detail: String,
}

impl AutoSetupStatus {
    /// Short, high-priority progress for the base status line. The full
    /// source-qualified summary remains available to inspectors, but a manual
    /// Analyze action must never look inert because optional status segments
    /// were crowded out.
    pub fn notice(&self) -> String {
        let mut value = format!("setup: {}", self.stage.label());
        if !self.detail.is_empty() {
            value.push_str(" · ");
            value.push_str(&self.detail);
        }
        value
    }

    pub fn summary(&self) -> String {
        let mut value = format!(
            "{} · automatic setup: {}",
            self.object_name,
            self.stage.label()
        );
        if !self.detail.is_empty() {
            value.push_str(" · ");
            value.push_str(&self.detail);
        }
        if let Some(session_id) = &self.session_id {
            value.push_str(" · Paseo session: ");
            value.push_str(session_id);
        }
        if self.stage == AutoSetupStage::Unavailable {
            value.push_str(" · retry: Ctrl-P → Current log › Analyze again");
        }
        value
    }
}

/// A request produced by an explicit palette action.  Automatic source-open
/// eligibility uses the same target shape when its settings/receipt adapter is
/// wired by the executable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AutoSetupRequest {
    pub source_id: String,
    pub origin_view_id: String,
    pub object_name: String,
    /// Semantic view fence.  Navigation and presentation-only interaction do
    /// not advance it.
    pub definition_revision: u64,
    /// Complete accepted definition. Proposal application compares this with
    /// the current native configuration; a hash is never overwrite authority.
    pub accepted_config: Box<AutoSetupViewConfig>,
    /// Bounded native validation feedback for an explicit retry on this log.
    /// Environment/transport diagnostics are never sent as model context.
    pub previous_failure: Option<String>,
}

/// The only operations the host may accept from an automatic setup proposal.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AutoSetupProposal {
    pub enrichments: Vec<EnrichmentDefinition>,
    pub pinned_columns: Vec<String>,
    pub color_rules: Vec<ColorRule>,
    pub grouping: String,
    pub severity_column: Option<String>,
    pub timestamp_column: Option<String>,
}

/// Full bounded review text for one schema-checked proposal.
///
/// Lists every enrichment expression, pin, colour rule, the grouping and the
/// severity/timestamp roles in proposal order, so the inspector can show the
/// complete candidate before anything is applied. Bounds come from the wire
/// proposal (at most eight enrichments, eight pins, sixteen colour rules and
/// one grouping); this function adds no truncation of its own. Wrapping and
/// scrolling are presentation-only folding in the inspector (AGENTS.md: the
/// query engine computes, the app names and presents).
pub fn format_proposal_summary(proposal: &AutoSetupProposal) -> String {
    let mut lines = Vec::new();
    lines.push(
        "Proposed Enhanced view — nothing applied yet. Review every line, then Apply.".to_owned(),
    );
    if proposal.enrichments.is_empty()
        && proposal.pinned_columns.is_empty()
        && proposal.color_rules.is_empty()
        && proposal.grouping.is_empty()
        && proposal.severity_column.is_none()
        && proposal.timestamp_column.is_none()
    {
        lines.push("No changes proposed; the raw view is kept.".to_owned());
        return lines.join("\n");
    }
    lines.push(format!("Enrichments ({})", proposal.enrichments.len()));
    if proposal.enrichments.is_empty() {
        lines.push("- none".to_owned());
    } else {
        for step in &proposal.enrichments {
            lines.push(format!("- {}: {}", step.id.0, step.source));
        }
    }
    if proposal.pinned_columns.is_empty() {
        lines.push("Pins (0): none".to_owned());
    } else {
        lines.push(format!(
            "Pins ({}) {}",
            proposal.pinned_columns.len(),
            proposal.pinned_columns.join(", ")
        ));
    }
    lines.push(format!("Colours ({})", proposal.color_rules.len()));
    if proposal.color_rules.is_empty() {
        lines.push("- none".to_owned());
    } else {
        for rule in &proposal.color_rules {
            lines.push(format!("- {} -> {}", rule.summary(), rule.color.label()));
        }
    }
    lines.push(format!(
        "Grouping: {}",
        describe_grouping(&proposal.grouping)
    ));
    let severity = proposal.severity_column.as_deref().unwrap_or("none");
    let timestamp = proposal.timestamp_column.as_deref().unwrap_or("none");
    lines.push(format!("Roles: severity={severity}; timestamp={timestamp}"));
    lines.push("Close keeps this proposal; Apply installs it as Enhanced.".to_owned());
    lines.join("\n")
}

fn describe_grouping(grouping: &str) -> String {
    if grouping.is_empty() {
        return "off".to_owned();
    }
    match crate::grouping::parse_grouping(grouping) {
        Ok(crate::grouping::GroupingSpec::Run { column }) => format!("run on {column}"),
        Ok(crate::grouping::GroupingSpec::Filter { column }) => format!("filter on {column}"),
        Ok(crate::grouping::GroupingSpec::Auto) => "auto".to_owned(),
        Ok(crate::grouping::GroupingSpec::Custom(rule)) => format!("custom {rule}"),
        Err(_) => grouping.to_owned(),
    }
}

/// Complete accepted configuration needed for exact edit-aware revert.
///
/// It intentionally retains ordinary recipe fields even though an automatic
/// proposal cannot set them: the pre-setup snapshot may contain presentation
/// choices on the canonical view, and revert must restore the exact snapshot.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AutoSetupViewConfig {
    pub recipe: RecipeConfig,
    pub color_rules: Vec<ColorRule>,
    pub severity_column: Option<String>,
    pub timestamp_column: Option<String>,
}

/// Session form of the durable receipt another module will persist.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AutoSetupReceipt {
    pub source_id: String,
    pub origin_view_id: String,
    pub generated_view_id: String,
    pub generated_view_name: String,
    pub before: AutoSetupViewConfig,
    pub applied: AutoSetupViewConfig,
}

/// Events the executable can hand to the future durable receipt adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AutoSetupEvent {
    Applied(Box<AutoSetupReceipt>),
    NoChanges {
        source_id: String,
        origin_view_id: String,
    },
    Unavailable {
        source_id: String,
        origin_view_id: String,
        diagnostic: String,
    },
    Failed {
        view_id: String,
        message: String,
    },
}

/// Versioned canonical bytes for a durable receipt's SHA-256.
///
/// The persistence layer owns the cryptographic digest and stores its
/// algorithm explicitly. In-memory overwrite safety always compares the full
/// [`AutoSetupViewConfig`] instead of trusting these bytes or their digest.
pub fn canonical_config_bytes(config: &AutoSetupViewConfig) -> Vec<u8> {
    let mut bytes = CanonicalBytes::new();
    bytes.text(&config.recipe.search);
    bytes.text(&config.recipe.advanced);
    bytes.text(&config.recipe.enrichment);
    bytes.usize(config.recipe.enrichments.len());
    for stage in &config.recipe.enrichments {
        bytes.text(&stage.id.0);
        bytes.text(&stage.source);
        match &stage.command {
            None => bytes.byte(0),
            Some(command) => {
                bytes.byte(1);
                bytes.command(command);
            }
        }
    }
    bytes.strings(&config.recipe.pinned_columns);
    bytes.optional_text(config.recipe.color_field.as_deref());
    bytes.optional_range(config.recipe.capture_time);
    bytes.optional_policy(config.recipe.capture_time_policy);
    bytes.time_basis(config.recipe.time_basis);
    bytes.text(&config.recipe.grouping);
    bytes.usize(config.color_rules.len());
    for rule in &config.color_rules {
        bytes.text(&rule.predicate);
        bytes.text(rule.color.label());
        bytes.optional_text(rule.column.as_deref());
        bytes.optional_text(rule.value.as_deref());
    }
    bytes.optional_text(config.severity_column.as_deref());
    bytes.optional_text(config.timestamp_column.as_deref());
    bytes.finish()
}

struct CanonicalBytes {
    value: Vec<u8>,
}

impl CanonicalBytes {
    fn new() -> Self {
        let mut value = Vec::with_capacity(256);
        value.extend_from_slice(b"lvu-auto-setup-config\0v1");
        Self { value }
    }

    fn byte(&mut self, byte: u8) {
        self.value.push(byte);
    }

    fn usize(&mut self, value: usize) {
        self.value.extend_from_slice(&(value as u64).to_le_bytes());
    }

    fn i64(&mut self, value: i64) {
        self.value.extend_from_slice(&value.to_le_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.value.extend_from_slice(&value.to_le_bytes());
    }

    fn text(&mut self, value: &str) {
        self.raw(value.as_bytes());
    }

    fn raw(&mut self, value: &[u8]) {
        self.usize(value.len());
        self.value.extend_from_slice(value);
    }

    fn optional_text(&mut self, value: Option<&str>) {
        match value {
            Some(value) => {
                self.byte(1);
                self.text(value);
            }
            None => self.byte(0),
        }
    }

    fn strings(&mut self, values: &[String]) {
        self.usize(values.len());
        for value in values {
            self.text(value);
        }
    }

    fn range(&mut self, range: CaptureTimeRange) {
        self.i64(range.start_unix_nanos);
        self.i64(range.end_unix_nanos);
    }

    fn optional_range(&mut self, range: Option<CaptureTimeRange>) {
        match range {
            Some(range) => {
                self.byte(1);
                self.range(range);
            }
            None => self.byte(0),
        }
    }

    fn optional_policy(&mut self, policy: Option<CaptureTimePolicy>) {
        match policy {
            None => self.byte(0),
            Some(CaptureTimePolicy::Absolute(range)) => {
                self.byte(1);
                self.range(range);
            }
            Some(CaptureTimePolicy::Recent { seconds }) => {
                self.byte(2);
                self.u64(seconds);
            }
        }
    }

    fn time_basis(&mut self, basis: TimeBasis) {
        self.byte(match basis {
            TimeBasis::Capture => 0,
            TimeBasis::Event => 1,
            TimeBasis::Extracted => 2,
            TimeBasis::Selected => 3,
        });
    }

    fn command(&mut self, command: &CommandDefinition) {
        match &command.program {
            CommandProgram::Shell { text } => {
                self.byte(0);
                self.text(text);
            }
            CommandProgram::Exec { executable, args } => {
                self.byte(1);
                self.raw(executable.as_os_str().as_encoded_bytes());
                self.strings(args);
            }
        }
        match &command.cwd {
            Some(path) => {
                self.byte(1);
                self.raw(path.as_os_str().as_encoded_bytes());
            }
            None => self.byte(0),
        }
        self.usize(command.environment.len());
        for (name, value) in &command.environment {
            self.text(name);
            self.text(value);
        }
        self.byte(match command.restart {
            RestartPolicy::Never => 0,
            RestartPolicy::OnFailure => 1,
            RestartPolicy::Always => 2,
        });
    }

    fn finish(self) -> Vec<u8> {
        self.value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::RuleColor;

    #[test]
    fn canonical_bytes_are_versioned_and_order_sensitive() {
        let mut first = AutoSetupViewConfig::default();
        first.recipe.enrichments = vec![
            EnrichmentDefinition::expression("one", "one = pl.lit(1)"),
            EnrichmentDefinition::expression("two", "two = pl.lit(2)"),
        ];
        first.color_rules.push(ColorRule {
            predicate: "one: 1".into(),
            color: RuleColor::Cyan,
            column: None,
            value: None,
        });
        let mut second = first.clone();
        second.recipe.enrichments.reverse();
        assert!(canonical_config_bytes(&first).starts_with(b"lvu-auto-setup-config\0v1"));
        assert_eq!(
            canonical_config_bytes(&first),
            canonical_config_bytes(&first.clone())
        );
        assert_ne!(
            canonical_config_bytes(&first),
            canonical_config_bytes(&second)
        );
        assert_ne!(first, second);

        let mut left = AutoSetupViewConfig::default();
        left.recipe.search = "ab".into();
        left.recipe.advanced = "c".into();
        let mut right = AutoSetupViewConfig::default();
        right.recipe.search = "a".into();
        right.recipe.advanced = "bc".into();
        assert_ne!(
            canonical_config_bytes(&left),
            canonical_config_bytes(&right)
        );
    }

    #[test]
    fn status_names_existing_object_before_operation() {
        let status = AutoSetupStatus {
            source_id: "source".into(),
            origin_view_id: "raw".into(),
            object_name: "payments / All events".into(),
            stage: AutoSetupStage::Analyzing,
            session_id: None,
            detail: "bounded sample".into(),
        };
        assert_eq!(
            status.summary(),
            "payments / All events · automatic setup: analyzing · bounded sample"
        );
    }

    #[test]
    fn unavailable_status_names_the_manual_retry_path() {
        let status = AutoSetupStatus {
            source_id: "source".into(),
            origin_view_id: "view".into(),
            object_name: "payments / All events".into(),
            stage: AutoSetupStage::Unavailable,
            session_id: None,
            detail: "agent service unavailable".into(),
        };
        let summary = status.summary();
        assert!(
            summary.contains("automatic setup: unavailable"),
            "{summary}"
        );
        assert!(
            summary.contains("Ctrl-P → Current log › Analyze again"),
            "{summary}"
        );
    }

    #[test]
    fn session_is_structured_and_named_in_the_full_summary() {
        let status = AutoSetupStatus {
            source_id: "source".into(),
            origin_view_id: "raw".into(),
            object_name: "payments / All events".into(),
            stage: AutoSetupStage::Analyzing,
            session_id: Some("paseo-auto-42".into()),
            detail: "bounded sample sent".into(),
        };
        assert!(status.summary().contains("Paseo session: paseo-auto-42"));
        assert!(status.notice().contains("setup: analyzing"));
        assert!(
            !status.notice().contains("paseo-auto-42"),
            "the compact acknowledgement must not hide its state behind a long id"
        );
    }

    #[test]
    fn review_stage_names_the_pending_review() {
        assert_eq!(AutoSetupStage::Review.label(), "review");
        let status = AutoSetupStatus {
            source_id: "source".into(),
            origin_view_id: "raw".into(),
            object_name: "payments / All events".into(),
            stage: AutoSetupStage::Review,
            session_id: Some("paseo-auto-42".into()),
            detail: "proposal ready for review".into(),
        };
        assert!(
            status.summary().contains("automatic setup: review"),
            "{}",
            status.summary()
        );
        assert!(
            status.notice().contains("setup: review"),
            "{}",
            status.notice()
        );
    }

    #[test]
    fn proposal_summary_lists_every_bounded_section() {
        let proposal = AutoSetupProposal {
            enrichments: vec![EnrichmentDefinition::expression(
                "severity-stage",
                "severity = pl.col('raw').str.extract(r'level=([A-Z]+)', 1)",
            )],
            pinned_columns: vec!["severity".into()],
            color_rules: vec![
                ColorRule::column_rule("severity".into(), "ERROR".into(), RuleColor::Red),
                ColorRule::column_rule("severity".into(), "WARN".into(), RuleColor::Yellow),
            ],
            grouping: crate::grouping::run_rule("severity"),
            severity_column: Some("severity".into()),
            timestamp_column: None,
        };
        let summary = format_proposal_summary(&proposal);
        assert!(summary.contains("nothing applied yet"), "{summary}");
        assert!(
            summary.contains("severity-stage") && summary.contains("severity = pl.col('raw')"),
            "{summary}"
        );
        assert!(
            summary.contains("Pins (1)") && summary.contains("severity"),
            "{summary}"
        );
        assert!(
            summary.contains("severity = ERROR -> red")
                && summary.contains("severity = WARN -> yellow"),
            "{summary}"
        );
        assert!(summary.contains("run on severity"), "{summary}");
        assert!(summary.contains("severity=severity"), "{summary}");
        assert!(summary.contains("Close keeps this proposal"), "{summary}");
    }

    #[test]
    fn empty_proposal_summary_keeps_raw_without_apply_language() {
        let summary = format_proposal_summary(&AutoSetupProposal::default());
        assert!(summary.contains("No changes proposed"), "{summary}");
        assert!(!summary.contains("Apply installs it"), "{summary}");
    }
}
