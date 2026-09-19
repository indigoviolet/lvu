//! Executable-side coordinator for automatic log setup.
//!
//! Protocol decoding and durable receipt storage are parallel concerns. They
//! integrate here through a bounded request/result seam rather than teaching
//! the terminal crate about bridge messages or SQLite.

use std::collections::{HashSet, VecDeque};

use lvu::{
    App, AutoSetupProposal, AutoSetupRequest, AutoSetupStage, AutoSetupStatus, ColorRule,
    EnrichmentDefinition, RuleColor,
};
use serde::Deserialize;

use crate::agent::{ProposalEnvelope, ProposalKind};

const MAX_PENDING_ANALYSES: usize = 1;
const AUTO_SETUP_SCHEMA_VERSION: u8 = 1;
const MAX_AUTO_SETUP_ENRICHMENTS: usize = 8;
const MAX_AUTO_SETUP_PINS: usize = 8;
const MAX_AUTO_SETUP_COLOR_RULES: usize = 16;
const MAX_AUTO_SETUP_ID_BYTES: usize = 128;
const MAX_AUTO_SETUP_OUTPUT_BYTES: usize = 64;
const MAX_AUTO_SETUP_EXPRESSION_BYTES: usize = 16 * 1024;
const MAX_AUTO_SETUP_COLOR_VALUE_BYTES: usize = 4096;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireAutoSetup {
    schema_version: u8,
    enrichments: Vec<WireEnrichment>,
    pinned_columns: Vec<String>,
    color_rules: Vec<WireColorRule>,
    grouping: Option<WireGrouping>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireEnrichment {
    id: String,
    output: String,
    expression: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireColorRule {
    column: String,
    value: String,
    color: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireGrouping {
    mode: String,
    column: String,
}

/// Convert a schema-validated bridge envelope into the existing native setup
/// primitives. This boundary validates again because `ProposalEnvelope` is a
/// public DTO and callers outside the bridge response path can construct one.
/// Native query compilation still proves expression semantics before the
/// composite candidate is published.
pub fn decode_auto_setup_proposal(
    envelope: &ProposalEnvelope,
) -> Result<AutoSetupProposal, String> {
    if envelope.kind != ProposalKind::AutoSetup {
        return Err("automatic setup response has the wrong proposal kind".into());
    }
    let wire: WireAutoSetup = serde_json::from_value(envelope.definition.clone())
        .map_err(|error| format!("invalid automatic setup definition: {error}"))?;
    if wire.schema_version != AUTO_SETUP_SCHEMA_VERSION {
        return Err(format!(
            "automatic setup schema version {} is unsupported",
            wire.schema_version
        ));
    }
    if wire.enrichments.len() > MAX_AUTO_SETUP_ENRICHMENTS {
        return Err(format!(
            "automatic setup has more than {MAX_AUTO_SETUP_ENRICHMENTS} enrichments"
        ));
    }

    let mut ids = HashSet::with_capacity(wire.enrichments.len());
    let mut outputs = HashSet::with_capacity(wire.enrichments.len());
    let mut enrichments = Vec::with_capacity(wire.enrichments.len());
    for enrichment in wire.enrichments {
        if enrichment.id.is_empty()
            || enrichment.id.len() > MAX_AUTO_SETUP_ID_BYTES
            || enrichment.id.chars().any(char::is_control)
        {
            return Err("automatic setup has an invalid enrichment id".into());
        }
        if !ids.insert(enrichment.id.clone()) {
            return Err(format!(
                "automatic setup repeats enrichment id {:?}",
                enrichment.id
            ));
        }
        if !valid_output(&enrichment.output) {
            return Err(format!(
                "automatic setup has invalid or protected output {:?}",
                enrichment.output
            ));
        }
        if !outputs.insert(enrichment.output.clone()) {
            return Err(format!(
                "automatic setup repeats output {:?}",
                enrichment.output
            ));
        }
        let native_source_len = enrichment
            .output
            .len()
            .checked_add(3)
            .and_then(|len| len.checked_add(enrichment.expression.len()));
        if enrichment.expression.is_empty()
            || native_source_len.is_none_or(|len| len > MAX_AUTO_SETUP_EXPRESSION_BYTES)
        {
            return Err(format!(
                "automatic setup expression for {:?} is empty or makes the native definition too large",
                enrichment.output
            ));
        }
        enrichments.push(EnrichmentDefinition::expression(
            enrichment.id,
            format!("{} = {}", enrichment.output, enrichment.expression),
        ));
    }

    if wire.pinned_columns.len() > MAX_AUTO_SETUP_PINS {
        return Err(format!(
            "automatic setup has more than {MAX_AUTO_SETUP_PINS} pinned columns"
        ));
    }
    let mut pins = HashSet::with_capacity(wire.pinned_columns.len());
    for column in &wire.pinned_columns {
        if !outputs.contains(column) {
            return Err(format!(
                "automatic setup pin {column:?} is not a proposed output"
            ));
        }
        if !pins.insert(column) {
            return Err(format!("automatic setup repeats pinned column {column:?}"));
        }
    }

    if wire.color_rules.len() > MAX_AUTO_SETUP_COLOR_RULES {
        return Err(format!(
            "automatic setup has more than {MAX_AUTO_SETUP_COLOR_RULES} colour rules"
        ));
    }
    let mut color_rules = Vec::with_capacity(wire.color_rules.len());
    for rule in wire.color_rules {
        if !outputs.contains(&rule.column) {
            return Err(format!(
                "automatic setup colour column {:?} is not a proposed output",
                rule.column
            ));
        }
        if rule.value.len() > MAX_AUTO_SETUP_COLOR_VALUE_BYTES {
            return Err(format!(
                "automatic setup colour value for {:?} is too large",
                rule.column
            ));
        }
        let color = RuleColor::parse(&rule.color).ok_or_else(|| {
            format!(
                "automatic setup colour token {:?} is unsupported",
                rule.color
            )
        })?;
        color_rules.push(ColorRule::column_rule(rule.column, rule.value, color));
    }

    let grouping = match wire.grouping {
        None => String::new(),
        Some(grouping) => {
            if !outputs.contains(&grouping.column) {
                return Err(format!(
                    "automatic setup grouping column {:?} is not a proposed output",
                    grouping.column
                ));
            }
            match grouping.mode.as_str() {
                "run" => lvu::grouping::run_rule(&grouping.column),
                "filter" => lvu::grouping::filter_rule(&grouping.column),
                other => {
                    return Err(format!(
                        "automatic setup grouping mode {other:?} is unsupported"
                    ));
                }
            }
        }
    };

    Ok(AutoSetupProposal {
        severity_column: outputs.contains("severity").then(|| "severity".into()),
        timestamp_column: outputs
            .contains("timestamp_utc")
            .then(|| "timestamp_utc".into()),
        enrichments,
        pinned_columns: wire.pinned_columns,
        color_rules,
        grouping,
    })
}

fn valid_output(output: &str) -> bool {
    !output.is_empty()
        && output.len() <= MAX_AUTO_SETUP_OUTPUT_BYTES
        && output != "raw"
        && !output.starts_with("_lvu_")
        && output
            .bytes()
            .all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
}

#[derive(Debug, Default)]
pub struct AutoSetupCoordinator {
    active: Option<AutoSetupAnalysis>,
    pending: VecDeque<AutoSetupRequest>,
}

/// Bridge-facing ticket. `data_revision` is supplied by the view/snapshot
/// adapter and changes when the source acquisition is replaced; scrolling and
/// selection do not change it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AutoSetupAnalysis {
    pub request: AutoSetupRequest,
    pub data_revision: String,
}

impl AutoSetupCoordinator {
    pub fn is_idle(&self) -> bool {
        self.active.is_none() && self.pending.is_empty()
    }

    /// Source-open integration hook. The caller invokes it only after the
    /// canonical raw view has at least one row and the durable receipt/settings
    /// policy says this source is eligible.
    pub fn enqueue_ready(
        &mut self,
        app: &mut App,
        origin_view_id: &str,
        object_name: String,
    ) -> bool {
        let Some(request) = app.auto_setup_request_for_view(origin_view_id, object_name) else {
            return false;
        };
        app.enqueue_auto_setup_request(request, AutoSetupStage::Sampling) && self.sync(app)
    }

    /// Drain UI requests without blocking rendering. At most one analysis is
    /// owned at a time; queue pressure is explicit and raw browsing continues.
    pub fn sync(&mut self, app: &mut App) -> bool {
        let mut changed = false;
        if let (Some(active), Some(status)) = (&self.active, app.auto_setup_status())
            && status.stage == AutoSetupStage::Unavailable
            && status.origin_view_id == active.request.origin_view_id
        {
            self.active = None;
            changed = true;
        }
        for request in app.take_auto_setup_requests() {
            changed = true;
            if self.active.is_some() || self.pending.len() >= MAX_PENDING_ANALYSES {
                app.set_auto_setup_status(AutoSetupStatus {
                    source_id: request.source_id,
                    origin_view_id: request.origin_view_id,
                    object_name: request.object_name,
                    stage: AutoSetupStage::Unavailable,
                    session_id: None,
                    detail: "another automatic analysis is active; raw view kept".into(),
                });
            } else {
                self.pending.push_back(request);
            }
        }
        changed
    }

    /// The protocol adapter takes one target and later returns its proposal.
    /// Taking it marks the one bounded active slot.
    pub fn take_analysis(
        &mut self,
        app: &mut App,
        data_revision: String,
    ) -> Option<AutoSetupAnalysis> {
        if self.active.is_some() {
            return None;
        }
        let request = self.pending.pop_front()?;
        app.set_auto_setup_status(AutoSetupStatus {
            source_id: request.source_id.clone(),
            origin_view_id: request.origin_view_id.clone(),
            object_name: request.object_name.clone(),
            stage: AutoSetupStage::Analyzing,
            session_id: None,
            detail: "bounded sample sent for typed setup".into(),
        });
        let analysis = AutoSetupAnalysis {
            request,
            data_revision,
        };
        self.active = Some(analysis.clone());
        Some(analysis)
    }

    /// Validate/apply the active proposal through lvu's native composite query
    /// transaction. JSON-schema validity at the bridge is not enough.
    pub fn complete(
        &mut self,
        app: &mut App,
        analysis: &AutoSetupAnalysis,
        current_data_revision: &str,
        result: Result<AutoSetupProposal, String>,
    ) -> Result<Option<String>, String> {
        if self.active.as_ref() != Some(analysis) {
            return Err("automatic setup result is stale or not owned".into());
        }
        self.active = None;
        let request = &analysis.request;
        if analysis.data_revision != current_data_revision {
            app.automatic_setup_unavailable(
                request,
                "source acquisition changed; analyze again".into(),
            );
            return Ok(None);
        }
        match result {
            Err(message) => {
                app.automatic_setup_unavailable(request, message);
                Ok(None)
            }
            Ok(proposal) => app
                .apply_auto_setup_proposal(request, proposal)
                .map(Some)
                .map_err(|error| format!("automatic setup refused: {error:?}")),
        }
    }

    pub fn active(&self) -> Option<&AutoSetupAnalysis> {
        self.active.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::OriginatingRevision;
    use lvu::{Action, SourceItem, ViewItem, ViewRole};
    use serde_json::{Value, json};

    fn app() -> App {
        let mut app = App::new(
            vec![SourceItem {
                id: "source".into(),
                name: "payments".into(),
                health: "running".into(),
            }],
            vec![ViewItem {
                id: "raw".into(),
                source_id: "source".into(),
                name: "All events".into(),
            }],
            false,
        );
        app.set_view_role("raw", ViewRole::Canonical);
        app
    }

    fn envelope(definition: Value) -> ProposalEnvelope {
        ProposalEnvelope {
            kind: ProposalKind::AutoSetup,
            definition,
            explanation: "Set up typed fields and presentation".into(),
            originating_revision: OriginatingRevision {
                data: "source:1:data:4".into(),
                definition: "view:7".into(),
            },
            needs_more_data: false,
        }
    }

    fn definition() -> Value {
        json!({
            "schema_version": 1,
            "enrichments": [
                {"id": "severity-stage", "output": "severity", "expression": "pl.col('raw').str.extract(r'level=([A-Z]+)', 1)"},
                {"id": "time-stage", "output": "timestamp_utc", "expression": "pl.col('observed_at').str.to_datetime(format='%+', strict=False).dt.convert_time_zone('UTC').dt.strftime('%Y-%m-%dT%H:%M:%S%.6fZ')"},
                {"id": "request-stage", "output": "request_id", "expression": "pl.col('raw').str.extract(r'request=([^ ]+)', 1)"}
            ],
            "pinned_columns": ["request_id"],
            "color_rules": [
                {"column": "severity", "value": "ERROR", "color": "red"},
                {"column": "severity", "value": "WARN", "color": "yellow"}
            ],
            "grouping": {"mode": "run", "column": "request_id"}
        })
    }

    #[test]
    fn wire_definition_lowers_only_to_existing_native_setup_types() {
        let proposal = decode_auto_setup_proposal(&envelope(definition())).unwrap();
        assert_eq!(proposal.enrichments.len(), 3);
        assert_eq!(proposal.enrichments[0].id.0, "severity-stage");
        assert_eq!(
            proposal.enrichments[0].source,
            "severity = pl.col('raw').str.extract(r'level=([A-Z]+)', 1)"
        );
        assert!(proposal.enrichments.iter().all(|step| !step.is_command()));
        assert_eq!(proposal.pinned_columns, ["request_id"]);
        assert_eq!(
            proposal.color_rules,
            [
                ColorRule::column_rule("severity".into(), "ERROR".into(), RuleColor::Red),
                ColorRule::column_rule("severity".into(), "WARN".into(), RuleColor::Yellow),
            ]
        );
        assert_eq!(proposal.grouping, lvu::grouping::run_rule("request_id"));
        assert_eq!(proposal.severity_column.as_deref(), Some("severity"));
        assert_eq!(proposal.timestamp_column.as_deref(), Some("timestamp_utc"));
    }

    #[test]
    fn empty_setup_and_exact_role_inference_are_supported() {
        let empty = envelope(json!({
            "schema_version": 1,
            "enrichments": [],
            "pinned_columns": [],
            "color_rules": [],
            "grouping": null
        }));
        assert_eq!(
            decode_auto_setup_proposal(&empty).unwrap(),
            AutoSetupProposal::default()
        );

        let near_names = envelope(json!({
            "schema_version": 1,
            "enrichments": [
                {"id": "level", "output": "level", "expression": "pl.lit('ERROR')"},
                {"id": "time", "output": "Timestamp_utc", "expression": "pl.lit(null)"}
            ],
            "pinned_columns": [],
            "color_rules": [],
            "grouping": null
        }));
        let proposal = decode_auto_setup_proposal(&near_names).unwrap();
        assert_eq!(proposal.severity_column, None);
        assert_eq!(proposal.timestamp_column, None);
    }

    #[test]
    fn wrong_kind_schema_or_forbidden_fields_are_rejected() {
        let mut wrong_kind = envelope(definition());
        wrong_kind.kind = ProposalKind::View;
        assert!(decode_auto_setup_proposal(&wrong_kind).is_err());

        for candidate in [
            json!({
                "schema_version": 2, "enrichments": [], "pinned_columns": [],
                "color_rules": [], "grouping": null
            }),
            json!({
                "schema_version": 1, "enrichments": [], "pinned_columns": [],
                "color_rules": [], "grouping": null, "filter": "ERROR"
            }),
            json!({
                "schema_version": 1,
                "enrichments": [{"id":"x","output":"x","expression":"pl.lit(1)","command":["sh"]}],
                "pinned_columns": [], "color_rules": [], "grouping": null
            }),
            json!({
                "schema_version": 1, "enrichments": [], "pinned_columns": [],
                "color_rules": [{"predicate":"ERROR","color":"red"}], "grouping": null
            }),
            json!({
                "schema_version": 1, "enrichments": [], "pinned_columns": [],
                "color_rules": [], "grouping": null, "severity_column": "raw"
            }),
        ] {
            assert!(
                decode_auto_setup_proposal(&envelope(candidate.clone())).is_err(),
                "accepted forbidden shape: {candidate}"
            );
        }
    }

    #[test]
    fn duplicate_and_oversized_collections_are_rejected() {
        let mut duplicate_id = definition();
        duplicate_id["enrichments"][1]["id"] = json!("severity-stage");
        let mut duplicate_output = definition();
        duplicate_output["enrichments"][1]["output"] = json!("severity");
        let mut duplicate_pin = definition();
        duplicate_pin["pinned_columns"] = json!(["request_id", "request_id"]);
        let over_enrichments = json!({
            "schema_version": 1,
            "enrichments": (0..=MAX_AUTO_SETUP_ENRICHMENTS).map(|index| json!({
                "id": format!("stage-{index}"), "output": format!("field_{index}"), "expression": "pl.lit(1)"
            })).collect::<Vec<_>>(),
            "pinned_columns": [], "color_rules": [], "grouping": null
        });
        let mut over_colors = definition();
        over_colors["color_rules"] = Value::Array(
            (0..=MAX_AUTO_SETUP_COLOR_RULES)
                .map(|_| json!({"column":"severity","value":"ERROR","color":"red"}))
                .collect(),
        );
        for candidate in [
            duplicate_id,
            duplicate_output,
            duplicate_pin,
            over_enrichments,
            over_colors,
        ] {
            assert!(decode_auto_setup_proposal(&envelope(candidate)).is_err());
        }
    }

    #[test]
    fn identifiers_expressions_and_values_keep_native_byte_bounds() {
        let mut long_id = definition();
        long_id["enrichments"][0]["id"] = json!("i".repeat(MAX_AUTO_SETUP_ID_BYTES + 1));
        let mut long_output = definition();
        long_output["enrichments"][0]["output"] =
            json!("o".repeat(MAX_AUTO_SETUP_OUTPUT_BYTES + 1));
        let mut long_expression = definition();
        long_expression["enrichments"][0]["expression"] =
            json!("x".repeat(MAX_AUTO_SETUP_EXPRESSION_BYTES));
        let mut long_value = definition();
        long_value["color_rules"][0]["value"] =
            json!("v".repeat(MAX_AUTO_SETUP_COLOR_VALUE_BYTES + 1));
        for candidate in [long_id, long_output, long_expression, long_value] {
            assert!(decode_auto_setup_proposal(&envelope(candidate)).is_err());
        }
    }

    #[test]
    fn native_references_colors_and_grouping_are_strict() {
        let mut candidates = Vec::new();
        let mut bad_output = definition();
        bad_output["enrichments"][0]["output"] = json!("_lvu_secret");
        candidates.push(bad_output);
        let mut foreign_pin = definition();
        foreign_pin["pinned_columns"] = json!(["foreign"]);
        candidates.push(foreign_pin);
        let mut foreign_color = definition();
        foreign_color["color_rules"][0]["column"] = json!("foreign");
        candidates.push(foreign_color);
        let mut bad_color = definition();
        bad_color["color_rules"][0]["color"] = json!("black");
        candidates.push(bad_color);
        let mut foreign_grouping = definition();
        foreign_grouping["grouping"]["column"] = json!("foreign");
        candidates.push(foreign_grouping);
        let mut automatic_grouping = definition();
        automatic_grouping["grouping"] = json!({"mode":"auto"});
        candidates.push(automatic_grouping);
        let mut internal_token = definition();
        internal_token["grouping"]["token"] = json!("(?lvu:run:v1:column:request_id)");
        candidates.push(internal_token);
        for candidate in candidates {
            assert!(decode_auto_setup_proposal(&envelope(candidate)).is_err());
        }

        let mut filter = definition();
        filter["grouping"] = json!({"mode":"filter","column":"request_id"});
        assert_eq!(
            decode_auto_setup_proposal(&envelope(filter))
                .unwrap()
                .grouping,
            lvu::grouping::filter_rule("request_id")
        );
    }

    #[test]
    fn coordinator_bounds_analysis_and_leaves_raw_view_selected() {
        let mut app = app();
        let (provider, _, _) = lvu::fixture::FixtureProvider::demo();
        app.handle(Action::AnalyzeAutoSetup, &provider);
        let mut coordinator = AutoSetupCoordinator::default();
        assert!(coordinator.sync(&mut app));
        let analysis = coordinator
            .take_analysis(&mut app, "acquisition:1".into())
            .expect("analysis");
        assert_eq!(app.active_view_id(), Some("raw"));
        assert_eq!(coordinator.active(), Some(&analysis));
        assert_eq!(
            app.auto_setup_status().map(|status| status.stage),
            Some(AutoSetupStage::Analyzing)
        );
    }

    #[test]
    fn source_replacement_stales_result_without_touching_raw_view() {
        let mut app = app();
        let (provider, _, _) = lvu::fixture::FixtureProvider::demo();
        app.handle(Action::AnalyzeAutoSetup, &provider);
        let mut coordinator = AutoSetupCoordinator::default();
        assert!(coordinator.sync(&mut app));
        let analysis = coordinator
            .take_analysis(&mut app, "acquisition:1".into())
            .expect("analysis");
        assert_eq!(
            coordinator
                .complete(
                    &mut app,
                    &analysis,
                    "acquisition:2",
                    Ok(AutoSetupProposal::default()),
                )
                .unwrap(),
            None
        );
        assert_eq!(app.active_view_id(), Some("raw"));
        assert_eq!(app.views().len(), 1);
        assert_eq!(
            app.auto_setup_status().map(|status| status.stage),
            Some(AutoSetupStage::Unavailable)
        );
    }
}
