//! UI-facing automatic log setup types.
//!
//! The bridge and durable receipt store deliberately do not live in this
//! crate.  This module is the narrow seam they hand validated proposals and
//! receipts through: it can describe only the existing native enrichment,
//! pin, colour-rule and grouping editors.  There is no field for a filter,
//! command, time window or source mutation.

use crate::app::{ColorRule, EnrichmentDefinition, RecipeConfig};

/// Bounded, non-modal progress shown while the raw view remains usable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AutoSetupStage {
    WaitingForRows,
    Sampling,
    Analyzing,
    Validating,
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
    pub detail: String,
}

impl AutoSetupStatus {
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
    pub accepted_digest: AutoSetupDigest,
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

/// Stable bounded fingerprint for receipt lookup and diagnostics.  Revert
/// also compares the full [`AutoSetupViewConfig`], so hash equality alone is
/// never authority for overwriting a manual edit.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct AutoSetupDigest(pub [u64; 2]);

/// Session form of the durable receipt another module will persist.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AutoSetupReceipt {
    pub source_id: String,
    pub origin_view_id: String,
    pub generated_view_id: String,
    pub generated_view_name: String,
    pub before: AutoSetupViewConfig,
    pub before_digest: AutoSetupDigest,
    pub applied: AutoSetupViewConfig,
    pub applied_digest: AutoSetupDigest,
}

/// Events the executable can hand to the future durable receipt adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AutoSetupEvent {
    Applied(Box<AutoSetupReceipt>),
    Reverted {
        generated_view_id: String,
        restored_digest: AutoSetupDigest,
    },
    Failed {
        view_id: String,
        message: String,
    },
}

/// Deterministic digest over the accepted native representation.
pub fn config_digest(config: &AutoSetupViewConfig) -> AutoSetupDigest {
    let mut digest = Digest::new();
    digest.text(&config.recipe.search);
    digest.text(&config.recipe.advanced);
    digest.text(&config.recipe.enrichment);
    digest.usize(config.recipe.enrichments.len());
    for stage in &config.recipe.enrichments {
        digest.text(&stage.id.0);
        digest.text(&stage.source);
        match &stage.command {
            None => digest.byte(0),
            Some(command) => {
                digest.byte(1);
                // Debug is not used as authority: full config equality is.
                // It does keep receipts for otherwise-equal command steps
                // distinct without duplicating lvu-core's wire encoder here.
                digest.text(&format!("{command:?}"));
            }
        }
    }
    digest.strings(&config.recipe.pinned_columns);
    digest.optional(config.recipe.color_field.as_deref());
    digest.text(&format!("{:?}", config.recipe.capture_time));
    digest.text(&format!("{:?}", config.recipe.capture_time_policy));
    digest.text(&format!("{:?}", config.recipe.time_basis));
    digest.text(&config.recipe.grouping);
    digest.usize(config.color_rules.len());
    for rule in &config.color_rules {
        digest.text(&rule.predicate);
        digest.text(&format!("{:?}", rule.color));
        digest.optional(rule.column.as_deref());
        digest.optional(rule.value.as_deref());
    }
    digest.optional(config.severity_column.as_deref());
    digest.optional(config.timestamp_column.as_deref());
    AutoSetupDigest([digest.left, digest.right])
}

struct Digest {
    left: u64,
    right: u64,
}

impl Digest {
    fn new() -> Self {
        Self {
            left: 0xcbf29ce484222325,
            right: 0x84222325cbf29ce4,
        }
    }

    fn byte(&mut self, byte: u8) {
        self.left ^= u64::from(byte);
        self.left = self.left.wrapping_mul(0x100000001b3);
        self.right ^= u64::from(byte).rotate_left(1);
        self.right = self.right.wrapping_mul(0x100000001b3).rotate_left(7);
    }

    fn usize(&mut self, value: usize) {
        for byte in (value as u64).to_le_bytes() {
            self.byte(byte);
        }
    }

    fn text(&mut self, value: &str) {
        self.usize(value.len());
        for byte in value.as_bytes() {
            self.byte(*byte);
        }
    }

    fn optional(&mut self, value: Option<&str>) {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::RuleColor;

    #[test]
    fn digest_is_order_sensitive_and_full_equality_remains_available() {
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
        assert_ne!(config_digest(&first), config_digest(&second));
        assert_ne!(first, second);
    }

    #[test]
    fn status_names_existing_object_before_operation() {
        let status = AutoSetupStatus {
            source_id: "source".into(),
            origin_view_id: "raw".into(),
            object_name: "payments / All events".into(),
            stage: AutoSetupStage::Analyzing,
            detail: "bounded sample".into(),
        };
        assert_eq!(
            status.summary(),
            "payments / All events · automatic setup: analyzing · bounded sample"
        );
    }
}
