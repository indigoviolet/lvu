use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lvu::{
    Action, App, AutoSetupProposal, AutoSetupStage, AutoSetupStatus, ColorRule,
    EnrichmentDefinition, QueryCompletion, QueryFailure, RuleColor, SourceItem, ViewDialogMode,
    ViewItem, ViewRole,
};
use ratatui::{Terminal, backend::TestBackend};

fn app_with_raw() -> (App, lvu::fixture::FixtureProvider) {
    let (provider, _, _) = lvu::fixture::FixtureProvider::demo();
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
    (app, provider)
}

fn proposal() -> AutoSetupProposal {
    AutoSetupProposal {
        enrichments: vec![EnrichmentDefinition::expression(
            "level-stage",
            "level = pl.col('raw').str.extract('level=(\\w+)', 1)",
        )],
        pinned_columns: vec!["level".into()],
        color_rules: vec![ColorRule::column_rule(
            "level".into(),
            "error".into(),
            RuleColor::Red,
        )],
        grouping: lvu::grouping::run_rule("level"),
        severity_column: Some("level".into()),
        timestamp_column: None,
    }
}

fn request(app: &mut App, provider: &lvu::fixture::FixtureProvider) -> lvu::AutoSetupRequest {
    app.handle(Action::AnalyzeAutoSetup, provider);
    app.take_auto_setup_requests()
        .into_iter()
        .next()
        .expect("analysis request")
}

fn settle_candidate(app: &mut App, candidate: &str, succeed: bool) {
    let fork = app.take_view_fork_requests();
    assert_eq!(fork.len(), 1);
    assert_eq!(fork[0].candidate_view_id, candidate);
    assert!(app.begin_fork_query(candidate));
    let mut queries = app.take_query_requests();
    assert_eq!(queries.len(), 1);
    let query = queries.remove(0);
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: query.view_id,
        generation: query.generation,
        revision: query.revision,
        purpose: query.purpose,
        result: if succeed {
            Ok(())
        } else {
            Err(QueryFailure {
                purpose: query.purpose,
                message: "controlled rejection".into(),
            })
        },
    }));
}

fn install_enhanced(app: &mut App, provider: &lvu::fixture::FixtureProvider) -> String {
    let analysis = request(app, provider);
    let candidate = app
        .apply_auto_setup_proposal(&analysis, proposal())
        .expect("proposal admitted");
    settle_candidate(app, &candidate, true);
    let ready = app.take_ready_forks();
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].candidate_view_id, candidate);
    assert!(app.install_fork(&candidate));
    candidate
}

#[test]
fn proposal_waits_for_explicit_review_and_close_keeps_it_pending() {
    let (mut app, provider) = app_with_raw();
    let analysis = request(&mut app, &provider);
    app.queue_auto_setup_proposal(&analysis, proposal())
        .unwrap();
    assert_eq!(
        app.auto_setup_status().unwrap().stage,
        AutoSetupStage::Review
    );
    assert_eq!(app.views().len(), 1);
    assert!(app.take_view_fork_requests().is_empty());
    assert!(app.take_query_requests().is_empty());
    app.handle(Action::OpenAutoSetupStatus, &provider);
    let mut terminal = Terminal::new(TestBackend::new(180, 40)).unwrap();
    terminal
        .draw(|frame| lvu::ui::render(frame, &mut app, &provider))
        .unwrap();
    let screen = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(screen.contains("level = pl.col"), "{screen}");
    assert!(screen.contains("Apply"), "{screen}");
    app.handle(
        Action::Raw(lvu::component::RawEvent::Key(KeyEvent::new(
            KeyCode::Esc,
            KeyModifiers::NONE,
        ))),
        &provider,
    );
    assert_eq!(app.views().len(), 1);
    assert!(app.take_auto_setup_apply_requests().is_empty());
    app.handle(Action::OpenAutoSetupStatus, &provider);
    app.handle(Action::ApplyAutoSetup, &provider);
    app.handle(Action::ApplyAutoSetup, &provider);
    let requests = app.take_auto_setup_apply_requests();
    assert_eq!(
        requests,
        vec![analysis.clone()],
        "Apply is admitted only once"
    );
    assert!(
        app.take_view_fork_requests().is_empty(),
        "runtime source fence precedes evaluation"
    );
    let candidate = app.apply_reviewed_auto_setup_proposal(&analysis).unwrap();
    settle_candidate(&mut app, &candidate, true);
    assert!(app.install_fork(&candidate));
    assert_eq!(app.views().len(), 2);
}

#[test]
fn narrow_review_scrolls_full_unicode_summary_without_applying() {
    let (mut app, provider) = app_with_raw();
    let analysis = request(&mut app, &provider);
    let mut candidate = proposal();
    candidate.enrichments[0].source = format!("level = pl.lit('{}')", "界e\u{301}".repeat(120));
    app.queue_auto_setup_proposal(&analysis, candidate).unwrap();
    app.handle(Action::OpenAutoSetupStatus, &provider);
    let mut terminal = Terminal::new(TestBackend::new(42, 16)).unwrap();
    terminal
        .draw(|frame| lvu::ui::render(frame, &mut app, &provider))
        .unwrap();
    for _ in 0..200 {
        app.handle(
            Action::Raw(lvu::component::RawEvent::Key(KeyEvent::new(
                KeyCode::Down,
                KeyModifiers::NONE,
            ))),
            &provider,
        );
    }
    terminal
        .draw(|frame| lvu::ui::render(frame, &mut app, &provider))
        .unwrap();
    let screen = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(
        screen.contains("Enhanced."),
        "the final summary line must be reachable: {screen}"
    );
    assert!(
        screen.contains("Apply") && screen.contains("Close"),
        "{screen}"
    );
    assert!(app.take_view_fork_requests().is_empty());
    assert!(app.take_auto_setup_apply_requests().is_empty());
}

#[test]
fn timestamp_setup_publishes_one_basis_and_revert_preserves_a_later_basis_edit() {
    let (mut app, provider) = app_with_raw();
    let analysis = request(&mut app, &provider);
    let mut candidate = proposal();
    candidate.enrichments.push(EnrichmentDefinition::expression(
        "event-time",
        "timestamp_utc = pl.col('ts')",
    ));
    candidate.timestamp_column = Some("timestamp_utc".into());
    let id = app.apply_auto_setup_proposal(&analysis, candidate).unwrap();
    assert_eq!(app.take_view_fork_requests().len(), 1);
    assert!(app.begin_fork_query(&id));
    let query = app.take_query_requests().pop().unwrap();
    assert_eq!(query.constraints.time_basis, lvu::TimeBasis::Selected);
    assert_eq!(
        query.constraints.time_field.as_deref(),
        Some("column:timestamp_utc|text|reject|-|%+")
    );
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: query.view_id,
        generation: query.generation,
        revision: query.revision,
        purpose: query.purpose,
        result: Ok(()),
    }));
    assert!(app.install_fork(&id));
    let state = app.view_state().unwrap();
    assert_eq!(state.applied_time_basis, lvu::TimeBasis::Selected);
    assert_eq!(
        state.applied_time_field.as_deref(),
        Some("column:timestamp_utc|text|reject|-|%+")
    );
    app.views.active_mut().unwrap().applied_time_field =
        Some("column:manual_time|text|reject|-|%+".into());
    app.handle(Action::RevertAutoSetup, &provider);
    assert!(app.layers.view.outbox.take().is_empty());
    assert!(
        app.action_notice
            .as_deref()
            .unwrap()
            .contains("revert refused")
    );
    // Older receipts assigned a timestamp role without selecting its basis.
    // That unchanged legacy configuration must remain revertible after upgrade.
    let mut legacy = app.auto_setup_receipt(&id).unwrap().clone();
    legacy.applied.recipe.time_basis = lvu::TimeBasis::Capture;
    let state = app.views.active_mut().unwrap();
    state.applied_time_basis = lvu::TimeBasis::Capture;
    state.applied_time_field = None;
    assert!(app.restore_auto_setup_receipt(legacy));
    app.handle(Action::RevertAutoSetup, &provider);
    let requests = app.layers.view.outbox.take();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].mode, ViewDialogMode::Delete);
}

#[test]
fn retry_discards_pending_review_and_stale_review_cannot_apply() {
    let (mut app, provider) = app_with_raw();
    let analysis = request(&mut app, &provider);
    app.queue_auto_setup_proposal(&analysis, proposal())
        .unwrap();
    app.handle(Action::ApplyAutoSetup, &provider);
    app.handle(Action::AnalyzeAutoSetup, &provider);
    assert!(app.take_auto_setup_apply_requests().is_empty());
    assert_eq!(
        app.apply_reviewed_auto_setup_proposal(&analysis),
        Err(lvu::AutoSetupRejected::Stale)
    );
    let next = app.take_auto_setup_requests().pop().unwrap();
    app.queue_auto_setup_proposal(&next, proposal()).unwrap();
    app.views
        .active_mut()
        .unwrap()
        .pinned_columns
        .push("manual edit".into());
    assert_eq!(
        app.apply_reviewed_auto_setup_proposal(&next),
        Err(lvu::AutoSetupRejected::Stale)
    );
    assert!(app.take_view_fork_requests().is_empty());
    assert_eq!(app.views().len(), 1);
}

#[test]
fn raw_first_atomic_success_and_native_editor_state() {
    let (mut app, provider) = app_with_raw();
    let analysis = request(&mut app, &provider);
    assert_eq!(app.active_view_id(), Some("raw"));
    assert_eq!(app.views().len(), 1, "raw opens before analysis settles");
    let candidate = app
        .apply_auto_setup_proposal(&analysis, proposal())
        .expect("proposal admitted");
    assert_eq!(app.views().len(), 1, "candidate is not partially visible");
    assert!(
        app.views
            .auto_setup_config("raw")
            .unwrap()
            .recipe
            .enrichments
            .is_empty()
    );
    settle_candidate(&mut app, &candidate, true);
    assert_eq!(
        app.views().len(),
        1,
        "query success still awaits persistence"
    );
    assert!(app.install_fork(&candidate));
    assert_eq!(app.active_view_id(), Some(candidate.as_str()));
    assert_eq!(app.views().len(), 2);
    let state = app.view_state().expect("enhanced state");
    assert_eq!(state.enrichments.len(), 1);
    assert_eq!(state.pinned_columns, ["level"]);
    assert_eq!(state.color_rules.len(), 1);
    assert_eq!(state.grouping.applied, lvu::grouping::run_rule("level"));
    assert_eq!(state.severity_column.as_deref(), Some("level"));
    assert!(app.auto_setup_revert_available());
}

#[test]
fn empty_proposal_records_no_changes_without_creating_a_view() {
    let (mut app, provider) = app_with_raw();
    let analysis = request(&mut app, &provider);
    assert_eq!(
        app.apply_auto_setup_proposal(&analysis, AutoSetupProposal::default())
            .unwrap(),
        "raw"
    );
    assert_eq!(app.views().len(), 1);
    assert!(app.take_view_fork_requests().is_empty());
    assert!(matches!(
        app.take_auto_setup_events().as_slice(),
        [lvu::AutoSetupEvent::NoChanges {
            source_id,
            origin_view_id,
        }] if source_id == "source" && origin_view_id == "raw"
    ));
    assert!(app.auto_setup_status().is_some_and(|status| {
        status.stage == AutoSetupStage::Applied && status.detail.contains("no useful")
    }));
}

#[test]
fn navigation_does_not_stale_analysis_and_late_success_does_not_steal_focus() {
    let (mut app, provider) = app_with_raw();
    let analysis = request(&mut app, &provider);
    let candidate = app
        .apply_auto_setup_proposal(&analysis, proposal())
        .expect("proposal admitted");
    app.handle(Action::ToggleFollow, &provider);
    app.add_source_view(
        SourceItem {
            id: "other-source".into(),
            name: "worker".into(),
            health: "running".into(),
        },
        ViewItem {
            id: "other-raw".into(),
            source_id: "other-source".into(),
            name: "All events".into(),
        },
    );
    app.set_view_role("other-raw", ViewRole::Canonical);
    app.select_view("other-raw");
    settle_candidate(&mut app, &candidate, true);
    assert!(app.install_fork(&candidate));
    assert_eq!(app.active_view_id(), Some("other-raw"));
    assert!(
        app.action_notice
            .as_deref()
            .is_some_and(|notice| notice.contains("current view kept"))
    );
}

#[test]
fn failures_and_invalid_proposals_keep_raw_last_good() {
    let (mut app, provider) = app_with_raw();
    let analysis = request(&mut app, &provider);
    let mut invalid = proposal();
    invalid.enrichments[0].command = Some(lvu_core::CommandDefinition {
        program: lvu_core::CommandProgram::Exec {
            executable: "false".into(),
            args: Vec::new(),
        },
        cwd: None,
        environment: Default::default(),
        restart: lvu_core::RestartPolicy::Never,
    });
    assert!(app.apply_auto_setup_proposal(&analysis, invalid).is_err());
    assert_eq!(app.views().len(), 1);
    assert_eq!(app.active_view_id(), Some("raw"));

    let analysis = request(&mut app, &provider);
    let candidate = app
        .apply_auto_setup_proposal(&analysis, proposal())
        .expect("valid proposal");
    settle_candidate(&mut app, &candidate, false);
    assert!(app.take_ready_forks().is_empty());
    assert_eq!(app.views().len(), 1);
    assert!(
        app.views
            .auto_setup_config("raw")
            .unwrap()
            .recipe
            .enrichments
            .is_empty()
    );
}

#[test]
fn full_accepted_config_is_the_staleness_fence() {
    let (mut app, provider) = app_with_raw();
    let mut analysis = request(&mut app, &provider);
    analysis.accepted_config.recipe.search = "different accepted definition".into();

    assert_eq!(
        app.apply_auto_setup_proposal(&analysis, proposal()),
        Err(lvu::AutoSetupRejected::Stale)
    );
    assert_eq!(app.active_view_id(), Some("raw"));
    assert_eq!(app.views().len(), 1);
    assert!(app.take_view_fork_requests().is_empty());
}

#[test]
fn bounded_request_queue_refusal_keeps_raw_usable() {
    let (mut app, provider) = app_with_raw();
    app.handle(Action::AnalyzeAutoSetup, &provider);
    app.handle(Action::AnalyzeAutoSetup, &provider);

    assert_eq!(app.take_auto_setup_requests().len(), 1);
    assert_eq!(app.active_view_id(), Some("raw"));
    assert_eq!(app.views().len(), 1);
    assert!(
        app.action_notice
            .as_deref()
            .is_some_and(|notice| notice.contains("already queued"))
    );
    assert_eq!(
        app.auto_setup_status().map(|status| status.stage),
        Some(AutoSetupStage::Sampling),
        "the first admitted request remains authoritative"
    );
}

#[test]
fn persistence_refusal_discards_candidate_and_reports_raw_fallback() {
    let (mut app, provider) = app_with_raw();
    let analysis = request(&mut app, &provider);
    let candidate = app
        .apply_auto_setup_proposal(&analysis, proposal())
        .expect("proposal admitted");
    settle_candidate(&mut app, &candidate, true);
    assert_eq!(app.take_ready_forks().len(), 1);

    assert!(app.discard_fork(&candidate, "workspace save failed".into()));
    assert_eq!(app.active_view_id(), Some("raw"));
    assert_eq!(app.views().len(), 1);
    assert!(app.auto_setup_status().is_some_and(|status| {
        status.stage == AutoSetupStage::Unavailable
            && status.detail.contains("workspace save failed")
            && status.detail.contains("raw view kept")
    }));
    assert!(matches!(
        app.take_auto_setup_events().last(),
        Some(lvu::AutoSetupEvent::Failed { message, .. })
            if message == "workspace save failed"
    ));
}

#[test]
fn names_are_unique_and_revert_refuses_after_manual_accepted_edit() {
    let (mut app, provider) = app_with_raw();
    let first = install_enhanced(&mut app, &provider);
    assert_eq!(
        app.views()
            .iter()
            .find(|view| view.id == first)
            .unwrap()
            .name,
        "Enhanced"
    );
    app.select_view("raw");
    let second = install_enhanced(&mut app, &provider);
    assert_eq!(
        app.views()
            .iter()
            .find(|view| view.id == second)
            .unwrap()
            .name,
        "Enhanced 2"
    );

    let mut edited = app.views.auto_setup_config(&second).unwrap().recipe;
    edited.pinned_columns.push("manual".into());
    app.views
        .apply_recipe(&second, edited, 0)
        .expect("manual edit queued");
    let query = app.take_query_requests().remove(0);
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: query.view_id,
        generation: query.generation,
        revision: query.revision,
        purpose: query.purpose,
        result: Ok(()),
    }));
    app.handle(Action::RevertAutoSetup, &provider);
    assert!(
        app.action_notice
            .as_deref()
            .is_some_and(|notice| notice.contains("edited") && notice.contains("refused"))
    );
    assert!(app.take_query_requests().is_empty());
    assert!(app.layers.view.outbox.take().is_empty());
}

#[test]
fn exact_revert_uses_normal_durable_view_deletion_and_preserves_raw() {
    let (mut app, provider) = app_with_raw();
    let enhanced = install_enhanced(&mut app, &provider);
    app.handle(Action::RevertAutoSetup, &provider);
    assert!(app.take_query_requests().is_empty());
    let requests = app.layers.view.outbox.take();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].mode, ViewDialogMode::Delete);
    assert_eq!(requests[0].source_id, "source");
    assert_eq!(requests[0].view_id, enhanced);
    assert_eq!(requests[0].name, "Enhanced");
    assert!(app.auto_setup_revert_available(), "durable ack is pending");
    assert!(app.views().iter().any(|view| view.id == enhanced));

    assert!(app.remove_view(&enhanced), "durable delete acknowledged");
    assert!(!app.auto_setup_revert_available());
    assert_eq!(app.active_view_id(), Some("raw"));
    assert_eq!(app.views().len(), 1);
    assert!(app.auto_setup_status().is_some_and(|status| {
        status.detail.contains("automatic setup removed")
            && status.detail.contains("raw capture preserved")
    }));
}

#[test]
fn non_modal_lifecycle_is_visible_in_test_backend() {
    let (mut app, provider) = app_with_raw();
    let _ = request(&mut app, &provider);
    let mut terminal = Terminal::new(TestBackend::new(180, 28)).unwrap();
    terminal
        .draw(|frame| lvu::ui::render(frame, &mut app, &provider))
        .unwrap();
    let screen = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(
        screen.contains("setup: sampling") && screen.contains("raw view remains usable"),
        "manual analysis must visibly acknowledge queueing:\n{screen}"
    );
    assert_eq!(app.active_view_id(), Some("raw"));
    assert_eq!(
        app.auto_setup_status().map(|status| status.stage),
        Some(AutoSetupStage::Sampling)
    );
}

#[test]
fn setup_inspector_names_the_log_session_and_live_manual_retry() {
    let (mut app, provider) = app_with_raw();
    assert!(app.set_auto_setup_status(AutoSetupStatus {
        source_id: "source".into(),
        origin_view_id: "raw".into(),
        object_name: "payments / All events".into(),
        stage: AutoSetupStage::Analyzing,
        session_id: Some("paseo-auto-session".into()),
        detail: "bounded sample sent for typed setup".into(),
    }));
    app.handle(Action::OpenAutoSetupStatus, &provider);
    assert_eq!(
        app.top_layer_action_labels(&provider),
        ["&Analyze again", "&Close"]
    );
    let mut terminal = Terminal::new(TestBackend::new(180, 32)).unwrap();
    terminal
        .draw(|frame| lvu::ui::render(frame, &mut app, &provider))
        .unwrap();
    let screen = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(screen.contains("Automatic setup status"), "{screen}");
    assert!(screen.contains("Log: payments / All events"), "{screen}");
    assert!(screen.contains("automatic setup: analyzing"), "{screen}");
    assert!(screen.contains("paseo-auto-session"), "{screen}");
    assert!(
        screen.contains("Analyze again") && screen.contains("Close"),
        "{screen}"
    );

    // The bridge may acquire a session or move stages while this inspector is
    // open. It must redraw that one authoritative lifecycle object rather
    // than leaving a stale modal over the live status line.
    assert!(app.set_auto_setup_status(AutoSetupStatus {
        source_id: "source".into(),
        origin_view_id: "raw".into(),
        object_name: "payments / All events".into(),
        stage: AutoSetupStage::Applying,
        session_id: Some("paseo-auto-session".into()),
        detail: "building Enhanced without changing All events".into(),
    }));
    terminal
        .draw(|frame| lvu::ui::render(frame, &mut app, &provider))
        .unwrap();
    let updated = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(updated.contains("automatic setup: applying"), "{updated}");
    assert!(updated.contains("building Enhanced"), "{updated}");

    // The visible A mnemonic is a real retry: it closes the inspector, queues
    // one bounded analysis and immediately publishes its sampling state.
    app.handle(
        Action::Raw(lvu::component::RawEvent::Key(KeyEvent::new(
            KeyCode::Char('a'),
            KeyModifiers::NONE,
        ))),
        &provider,
    );
    assert_eq!(app.take_auto_setup_requests().len(), 1);
    assert_eq!(
        app.auto_setup_status().map(|status| status.stage),
        Some(AutoSetupStage::Sampling)
    );
    assert!(app.action_notice.as_deref().is_some_and(|notice| {
        notice.contains("setup: sampling") && notice.contains("raw view remains usable")
    }));
}

#[test]
fn palette_names_existing_object_before_operation_and_gates_revert() {
    use lvu::command_palette::{CommandId, Palette, PaletteContext};

    let mut context = PaletteContext::new(lvu::Focus::Logs, true);
    let mut palette = Palette::new();
    palette.open(context.clone());
    let analyze = palette
        .commands()
        .iter()
        .find(|command| command.id == CommandId::AnalyzeAutoSetup)
        .unwrap();
    assert_eq!(analyze.name, "Current log › Analyze again");
    let revert = palette
        .commands()
        .iter()
        .find(|command| command.id == CommandId::RevertAutoSetup)
        .unwrap();
    assert!(!revert.is_enabled());

    context.auto_setup_revert_available = true;
    palette.refresh_context(context);
    assert!(
        palette
            .commands()
            .iter()
            .find(|command| command.id == CommandId::RevertAutoSetup)
            .unwrap()
            .is_enabled()
    );
}
