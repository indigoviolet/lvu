//! Executable-side coordinator for automatic log setup.
//!
//! Protocol decoding and durable receipt storage are parallel concerns. They
//! integrate here through a bounded request/result seam rather than teaching
//! the terminal crate about bridge messages or SQLite.

use std::collections::VecDeque;

use lvu::{App, AutoSetupProposal, AutoSetupRequest, AutoSetupStage, AutoSetupStatus};

const MAX_PENDING_ANALYSES: usize = 1;

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
        for request in app.take_auto_setup_requests() {
            changed = true;
            if self.active.is_some() || self.pending.len() >= MAX_PENDING_ANALYSES {
                app.set_auto_setup_status(AutoSetupStatus {
                    source_id: request.source_id,
                    origin_view_id: request.origin_view_id,
                    object_name: request.object_name,
                    stage: AutoSetupStage::Unavailable,
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
            app.set_auto_setup_status(AutoSetupStatus {
                source_id: request.source_id.clone(),
                origin_view_id: request.origin_view_id.clone(),
                object_name: request.object_name.clone(),
                stage: AutoSetupStage::Unavailable,
                detail: "source acquisition changed; analyze again".into(),
            });
            return Ok(None);
        }
        match result {
            Err(message) => {
                app.set_auto_setup_status(AutoSetupStatus {
                    source_id: request.source_id.clone(),
                    origin_view_id: request.origin_view_id.clone(),
                    object_name: request.object_name.clone(),
                    stage: AutoSetupStage::Unavailable,
                    detail: format!("{message}; raw view kept"),
                });
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
    use lvu::{Action, SourceItem, ViewItem, ViewRole};

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
