//! Per-view enrichment-column roles: AI shortcuts produce ordinary reviewed
//! definitions, and a role feeds display only while the accepted chain
//! produces an output of that name. Roles are explicit and unset by default;
//! nothing is guessed from raw field names.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lvu::{
    Action, App, AskAiKind, QueryCompletion, QueryFailure, QueryPurpose, RowProvider,
    app::{
        AskTask, PendingRoleIntent, PendingRoleOutcome, SEVERITY_PROMPT, TIMESTAMP_PROMPT,
        ViewState, basis_covers_role, consume_pending_role, effective_role, enrichment_output_name,
        role_time_token,
    },
    component::{Open, RawEvent},
    fixture::FixtureProvider,
};

fn demo() -> (FixtureProvider, App) {
    let (provider, sources, views) = FixtureProvider::demo();
    (provider, App::new(sources, views, true))
}

fn key(app: &mut App, provider: &impl RowProvider, code: KeyCode) {
    app.handle(
        Action::Raw(RawEvent::Key(KeyEvent::new(code, KeyModifiers::NONE))),
        provider,
    );
}

fn alt(app: &mut App, provider: &impl RowProvider, code: KeyCode) {
    app.handle(
        Action::Raw(RawEvent::Key(KeyEvent::new(code, KeyModifiers::ALT))),
        provider,
    );
}

fn paste(app: &mut App, provider: &impl RowProvider, text: &str) {
    app.handle(Action::Raw(RawEvent::Paste(text.into())), provider);
}

fn outputs(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| name.to_string()).collect()
}

#[test]
fn tasks_wire_prompts_to_their_intended_outputs() {
    // One timestamp task carries the reviewed timestamp contract; the former
    // duplicate `Recognize timestamp` route now opens the same task.
    assert_eq!(AskTask::TimestampColumn.prompt(), TIMESTAMP_PROMPT);
    assert_eq!(AskTask::SeverityColumn.prompt(), SEVERITY_PROMPT);
    assert_eq!(AskTask::SeverityColumn.intended_column(), "severity");
    assert_eq!(AskTask::TimestampColumn.intended_column(), "timestamp_utc");
}

#[test]
fn severity_prompt_states_its_output_contract() {
    // The renderer maps canonical tokens to colour without normalizing, so
    // the prompt must demand exactly those tokens and nulls elsewhere.
    assert!(SEVERITY_PROMPT.contains("named severity"));
    for token in ["TRACE", "DEBUG", "INFO", "WARN", "ERROR", "FATAL"] {
        assert!(
            SEVERITY_PROMPT.contains(token),
            "missing canonical token {token}"
        );
    }
    assert!(SEVERITY_PROMPT.contains("must produce null"));
    assert!(SEVERITY_PROMPT.contains("do not invent"));
    // Ordinary unstructured logs are in scope: raw extraction is the
    // documented fallback, exactly as in the timestamp contract.
    assert!(SEVERITY_PROMPT.contains("raw extraction"));
}

#[test]
fn roles_resolve_only_while_chain_produces_them() {
    let chain = outputs(&["severity", "timestamp_utc"]);
    let mut state = ViewState::default();
    // Unset by default: nothing guessed, even with outputs present.
    assert_eq!(state.effective_roles(&chain), (None, None));
    assert_eq!(state.effective_roles(&[]), (None, None));
    state.severity_column = Some("severity".into());
    state.timestamp_column = Some("timestamp_utc".into());
    assert_eq!(
        state.effective_roles(&chain),
        (Some("severity".into()), Some("timestamp_utc".into()))
    );
    // Removing an output invalidates its role while the other sticks: an
    // invalid role falls back to raw display instead of erroring the view,
    // which is what keeps the last good view usable.
    assert_eq!(
        state.effective_roles(&outputs(&["timestamp_utc"])),
        (None, Some("timestamp_utc".into()))
    );
    assert_eq!(state.effective_roles(&[]), (None, None));
}

#[test]
fn roles_never_guess_raw_field_names() {
    let mut state = ViewState::default();
    state.severity_column = Some("level".into());
    // `level` exists in the raw display fields, but no accepted enrichment
    // produces it: the role stays unset rather than silently reading raw.
    assert_eq!(state.effective_roles(&outputs(&["severity"])), (None, None));
}

#[test]
fn role_names_match_exactly() {
    // Enrichment outputs are case-sensitive; a near miss must not resolve.
    assert_eq!(
        effective_role(Some("Severity"), &outputs(&["severity"])),
        None
    );
    assert_eq!(
        effective_role(Some("severity "), &outputs(&["severity"])),
        None
    );
    assert_eq!(
        effective_role(Some("severity"), &outputs(&["severity"])),
        Some("severity".into())
    );
}

#[test]
fn timestamp_role_stages_a_selected_basis_token() {
    // The staged draft travels the normal Time candidate fences: five
    // `|`-separated parts, the column reference, a text interpretation, the
    // recognition flow's own RFC 3339 shape and a reject assumption for
    // zoneless values. A malformed token fails with the applied basis
    // untouched.
    assert_eq!(
        role_time_token("event_time"),
        "column:event_time|text|reject|-|%+"
    );
}

#[test]
fn timestamp_gutter_honors_the_display_zone() {
    // The gutter formats the row's validated `basis_nanos` instant through
    // the same display-zone formatter as capture time: a non-UTC zone must
    // not silently fall back to the enrichment's UTC text.
    let nanos = 1_788_611_450_000_000_000;
    let utc = lvu::app::format_display_time(nanos, "UTC");
    let shifted = lvu::app::format_display_time(nanos, "+02:00");
    assert!(utc.ends_with('Z'), "UTC keeps its Z suffix: {utc}");
    assert!(
        shifted.ends_with("+02:00"),
        "a configured offset travels to the gutter: {shifted}"
    );
    assert_ne!(utc, shifted);
}

#[test]
fn enrichment_output_names_come_from_assignment_form_only() {
    // Mirrors the compiler's documented first step, so an assignment the
    // chain accepts parses here exactly as it does there.
    assert_eq!(
        enrichment_output_name("severity = pl.col(\"level\")"),
        Some("severity".into())
    );
    assert_eq!(
        enrichment_output_name("  padded  = pl.lit(1)  "),
        Some("padded".into())
    );
    // Slash shorthand never splits into a name: those steps apply normally
    // but never auto-assign a role, and Fields remains the way to name one.
    assert_eq!(enrichment_output_name("/(?P<severity>\\w+)/"), None);
    assert_eq!(enrichment_output_name("no equals here"), None);
    assert_eq!(enrichment_output_name(" = pl.lit(1)"), None);
}

#[test]
fn pending_intent_settles_only_on_newer_completions() {
    let intent = Some(PendingRoleIntent {
        column: "severity".into(),
        severity: true,
        after_revision: 7,
    });
    let chain = ["severity".to_string()];
    // Older in-flight completions predate the intent and leave it alone.
    assert_eq!(
        consume_pending_role(&intent, 7, true, &chain),
        PendingRoleOutcome::Ignore
    );
    assert_eq!(
        consume_pending_role(&intent, 3, false, &chain),
        PendingRoleOutcome::Ignore
    );
    // A newer success carrying the intended output assigns it.
    assert_eq!(
        consume_pending_role(&intent, 8, true, &chain),
        PendingRoleOutcome::Assign("severity".into())
    );
    // Failure, rename, and unrelated newer chains clear without assigning.
    assert_eq!(
        consume_pending_role(&intent, 8, false, &chain),
        PendingRoleOutcome::Clear
    );
    assert_eq!(
        consume_pending_role(&intent, 8, true, &["other".to_string()]),
        PendingRoleOutcome::Clear
    );
    assert_eq!(
        consume_pending_role(&intent, 8, true, &[]),
        PendingRoleOutcome::Clear
    );
    assert_eq!(
        consume_pending_role(&None, 8, true, &chain),
        PendingRoleOutcome::Ignore
    );
}

#[test]
fn basis_covers_role_needs_selected_basis_on_the_same_column() {
    use lvu::TimeBasis;
    let token = role_time_token("event_time");
    assert!(basis_covers_role(
        TimeBasis::Selected,
        Some(&token),
        Some("event_time")
    ));
    // Anything else leaves the gutter off event time: capture basis, a
    // different column, a missing or unparsable token, or no role at all.
    assert!(!basis_covers_role(
        TimeBasis::Capture,
        Some(&token),
        Some("event_time")
    ));
    assert!(!basis_covers_role(
        TimeBasis::Selected,
        Some(&role_time_token("other")),
        Some("event_time")
    ));
    assert!(!basis_covers_role(
        TimeBasis::Selected,
        None,
        Some("event_time")
    ));
    assert!(!basis_covers_role(
        TimeBasis::Selected,
        Some("not-a-token"),
        Some("event_time")
    ));
    assert!(!basis_covers_role(TimeBasis::Selected, Some(&token), None));
    assert!(!basis_covers_role(
        TimeBasis::Selected,
        Some(&token),
        Some("")
    ));
}

#[test]
fn shortcut_intent_assigns_on_matching_accepted_output() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Enrichment), &provider);
    alt(&mut app, &provider, KeyCode::Char('a'));
    paste(&mut app, &provider, "severity = pl.lit('ok')");
    // The intent arrives with the reviewed proposal, before the step saves.
    let after = app.view_state().unwrap().desired_query_revision;
    app.views.active_mut().unwrap().pending_role = Some(PendingRoleIntent {
        column: "severity".into(),
        severity: true,
        after_revision: after,
    });
    key(&mut app, &provider, KeyCode::Enter);
    let request = app.take_query_requests().pop().unwrap();
    assert!(request.revision > after);
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: request.view_id.clone(),
        generation: request.generation,
        revision: request.revision,
        purpose: QueryPurpose::Enrichment,
        result: Ok(()),
    }));
    let state = app.view_state().unwrap();
    assert_eq!(state.severity_column.as_deref(), Some("severity"));
    assert!(state.pending_role.is_none());
}

#[test]
fn renamed_output_does_not_assign() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Enrichment), &provider);
    alt(&mut app, &provider, KeyCode::Char('a'));
    paste(&mut app, &provider, "loudness = pl.lit('ok')");
    let after = app.view_state().unwrap().desired_query_revision;
    app.views.active_mut().unwrap().pending_role = Some(PendingRoleIntent {
        column: "severity".into(),
        severity: true,
        after_revision: after,
    });
    key(&mut app, &provider, KeyCode::Enter);
    let request = app.take_query_requests().pop().unwrap();
    assert!(request.revision > after);
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: request.view_id.clone(),
        generation: request.generation,
        revision: request.revision,
        purpose: QueryPurpose::Enrichment,
        result: Ok(()),
    }));
    // The step applied — the role did not follow the renamed output.
    let state = app.view_state().unwrap();
    assert!(state.severity_column.is_none());
    assert!(state.pending_role.is_none());
    assert!(
        state
            .enrichments
            .iter()
            .any(|step| step.source.starts_with("loudness = "))
    );
}

#[test]
fn failed_completion_clears_without_touching_roles() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Enrichment), &provider);
    alt(&mut app, &provider, KeyCode::Char('a'));
    paste(&mut app, &provider, "broken = pl.col(");
    {
        let state = app.views.active_mut().unwrap();
        let after = state.desired_query_revision;
        state.severity_column = Some("old".into());
        state.pending_role = Some(PendingRoleIntent {
            column: "severity".into(),
            severity: true,
            after_revision: after,
        });
    }
    key(&mut app, &provider, KeyCode::Enter);
    let request = app.take_query_requests().pop().unwrap();
    app.apply_query_completion(QueryCompletion {
        view_id: request.view_id,
        generation: request.generation,
        revision: request.revision,
        purpose: QueryPurpose::Enrichment,
        result: Err(QueryFailure {
            purpose: QueryPurpose::Enrichment,
            message: "synthetic failure".into(),
        }),
    });
    let state = app.view_state().unwrap();
    assert_eq!(state.severity_column.as_deref(), Some("old"));
    assert!(state.pending_role.is_none());
}

#[test]
fn stale_completion_keeps_pending_and_roles() {
    let (_provider, mut app) = demo();
    {
        let state = app.views.active_mut().unwrap();
        state.enrichment.pending_generation = Some(7);
        state.desired_query_revision = 5;
        state.pending_role = Some(PendingRoleIntent {
            column: "severity".into(),
            severity: true,
            after_revision: 9,
        });
    }
    // An older completion cannot answer the newer intent.
    app.apply_query_completion(QueryCompletion {
        view_id: app.active_view_id().unwrap().to_owned(),
        generation: 7,
        revision: 5,
        purpose: QueryPurpose::Enrichment,
        result: Ok(()),
    });
    let state = app.view_state().unwrap();
    assert!(state.pending_role.is_some());
    assert!(state.severity_column.is_none());
}

#[test]
fn fork_transfers_pending_intent_and_leaves_origin_quiet() {
    let (provider, mut app) = demo();
    let origin = app.active_view_id().unwrap().to_owned();
    app.set_view_role(&origin, lvu::ViewRole::Canonical);
    // A reviewed shortcut proposal stages a fork: the intent is single-owner,
    // so it moves from the origin to that one candidate and the origin goes
    // quiet. A later unrelated fork must not revive it.
    app.handle(
        Action::ApplyAskProposal {
            kind: AskAiKind::Enrichment,
            expression: "severity = pl.lit('ok')".into(),
            recipe: None,
            outcome: None,
            task: Some(AskTask::SeverityColumn),
        },
        &provider,
    );
    let staged = app.take_view_fork_requests();
    assert_eq!(staged.len(), 1);
    let candidate = staged[0].candidate_view_id.clone();
    assert!(
        app.views.state(&origin).unwrap().pending_role.is_none(),
        "the origin must not stay armed after staging"
    );
    let pending = app
        .views
        .state(&candidate)
        .unwrap()
        .pending_role
        .clone()
        .expect("the candidate owns the transferred intent");
    assert_eq!(pending.column, "severity");
    assert!(pending.severity);
    assert_eq!(pending.after_revision, 0);
    assert!(app.begin_fork_query(&candidate));
    let query = app
        .take_query_requests()
        .into_iter()
        .find(|request| request.view_id == candidate)
        .expect("the candidate queries for itself");
    assert!(query.revision > 0);
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: query.view_id.clone(),
        generation: query.generation,
        revision: query.revision,
        purpose: QueryPurpose::Enrichment,
        result: Ok(()),
    }));
    // The fork's own acceptance assigns the role.
    let state = app.views.state(&candidate).unwrap();
    assert_eq!(state.severity_column.as_deref(), Some("severity"));
    assert!(state.pending_role.is_none());
    // Installing the candidate leaves the origin quiet.
    assert!(app.install_fork(&candidate));
    assert!(
        app.views.state(&origin).unwrap().pending_role.is_none(),
        "install must not leave the origin armed"
    );
    // A later unrelated fork from the same origin carries no stale intent:
    // its acceptance must not assign a role without a new shortcut.
    app.select_view(&origin);
    app.handle(Action::Open(Open::Enrichment), &provider);
    alt(&mut app, &provider, KeyCode::Char('a'));
    paste(&mut app, &provider, "loudness = pl.lit('ok')");
    key(&mut app, &provider, KeyCode::Enter);
    let staged = app.take_view_fork_requests();
    assert_eq!(staged.len(), 1);
    let second = staged[0].candidate_view_id.clone();
    assert!(
        app.views.state(&second).unwrap().pending_role.is_none(),
        "an unrelated fork must not inherit a stale intent"
    );
    assert!(app.begin_fork_query(&second));
    let query = app
        .take_query_requests()
        .into_iter()
        .find(|request| request.view_id == second)
        .expect("the second candidate queries for itself");
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: query.view_id.clone(),
        generation: query.generation,
        revision: query.revision,
        purpose: QueryPurpose::Enrichment,
        result: Ok(()),
    }));
    let state = app.views.state(&second).unwrap();
    assert!(state.severity_column.is_none());
    assert!(state.pending_role.is_none());
}

#[test]
fn discarded_fork_clears_pending_intent() {
    let (provider, mut app) = demo();
    let origin = app.active_view_id().unwrap().to_owned();
    app.set_view_role(&origin, lvu::ViewRole::Canonical);
    app.handle(
        Action::ApplyAskProposal {
            kind: AskAiKind::Enrichment,
            expression: "severity = pl.lit('ok')".into(),
            recipe: None,
            outcome: None,
            task: Some(AskTask::SeverityColumn),
        },
        &provider,
    );
    let staged = app.take_view_fork_requests();
    assert_eq!(staged.len(), 1);
    let candidate = staged[0].candidate_view_id.clone();
    assert!(app.begin_fork_query(&candidate));
    let query = app
        .take_query_requests()
        .into_iter()
        .find(|request| request.view_id == candidate)
        .expect("the candidate queries for itself");
    // A failed fork discards the candidate and clears the origin: the
    // transferred intent dies with the fork it was moved to.
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: query.view_id.clone(),
        generation: query.generation,
        revision: query.revision,
        purpose: QueryPurpose::Enrichment,
        result: Err(QueryFailure {
            purpose: QueryPurpose::Enrichment,
            message: "synthetic failure".into(),
        }),
    }));
    assert!(
        app.views.state(&origin).unwrap().pending_role.is_none(),
        "discard must not leave the origin armed"
    );
    assert!(app.views.state(&candidate).is_none());
}

#[test]
fn role_toggle_refuses_columns_without_provenance() {
    let (provider, mut app) = demo();
    app.sync_provider(&provider, 10);
    app.handle(Action::Open(Open::Fields), &provider);
    // Fixture rows carry no derived markers, so no column here can feed a
    // role: the toggle refuses with a reason instead of recording the name.
    key(&mut app, &provider, KeyCode::Char('s'));
    let state = app.view_state().unwrap();
    assert!(state.severity_column.is_none());
    assert!(
        app.action_notice.as_deref().is_some_and(
            |notice| notice.contains("only an accepted enrichment output can feed a role")
        ),
        "refusal must say why: {:?}",
        app.action_notice
    );
    key(&mut app, &provider, KeyCode::Char('t'));
    let state = app.view_state().unwrap();
    assert!(state.timestamp_column.is_none());
}

#[test]
fn restore_submits_chain_basis_and_roles_together() {
    use lvu::{PersistentViewState, TimeBasis};
    let (_provider, mut app) = demo();
    let view = app.active_view_id().unwrap().to_owned();
    assert!(app.restore_persistent_view(
        &view,
        PersistentViewState {
            applied_enrichments: vec![lvu::EnrichmentDefinition {
                id: lvu::EnrichmentStageId("copy".into()),
                source: "severity = pl.col(\"level\")".into(),
                command: None,
            }],
            severity_column: Some("severity".into()),
            timestamp_column: Some("event_time".into()),
            applied_time_basis: TimeBasis::Selected,
            applied_time_field: Some(role_time_token("event_time")),
            ..PersistentViewState::default()
        }
    ));
    // The restored view resubmits one query carrying the chain and the
    // converged basis together: restore never splits them apart.
    let request = app.take_query_requests().pop().unwrap();
    assert_eq!(request.constraints.enrichments.len(), 1);
    assert_eq!(request.constraints.time_basis, TimeBasis::Selected);
    assert_eq!(
        request.constraints.time_field.as_deref(),
        Some(role_time_token("event_time").as_str())
    );
    // Roles land in session state from the same restore.
    let state = app.view_state().unwrap();
    assert_eq!(state.severity_column.as_deref(), Some("severity"));
    assert_eq!(state.timestamp_column.as_deref(), Some("event_time"));
}

#[test]
fn refused_generic_proposal_preserves_an_older_shortcut_intent() {
    let (provider, mut app) = demo();
    // Arm shortcut intent A directly: it awaits a newer accepted chain
    // publishing `severity`.
    let after = app.view_state().unwrap().desired_query_revision;
    app.views.active_mut().unwrap().pending_role = Some(PendingRoleIntent {
        column: "severity".into(),
        severity: true,
        after_revision: after,
    });
    // A generic enrichment proposal with an empty expression is refused at
    // enqueue (nothing stages, no request runs). It installed no intent, so
    // the older one must survive the refusal untouched.
    app.handle(
        Action::ApplyAskProposal {
            kind: AskAiKind::Enrichment,
            expression: String::new(),
            recipe: None,
            outcome: None,
            task: None,
        },
        &provider,
    );
    assert!(
        app.take_query_requests().is_empty(),
        "a refused proposal stages no request"
    );
    assert_eq!(
        app.view_state().unwrap().pending_role,
        Some(PendingRoleIntent {
            column: "severity".into(),
            severity: true,
            after_revision: after,
        }),
        "a generic refusal must not clear an older shortcut intent"
    );
    // A's own later acceptance still assigns the role.
    app.handle(Action::Open(Open::Enrichment), &provider);
    alt(&mut app, &provider, KeyCode::Char('a'));
    paste(&mut app, &provider, "severity = pl.lit('ok')");
    key(&mut app, &provider, KeyCode::Enter);
    let request = app.take_query_requests().pop().unwrap();
    assert!(request.revision > after);
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: request.view_id.clone(),
        generation: request.generation,
        revision: request.revision,
        purpose: QueryPurpose::Enrichment,
        result: Ok(()),
    }));
    let state = app.view_state().unwrap();
    assert_eq!(state.severity_column.as_deref(), Some("severity"));
    assert!(state.pending_role.is_none());
}

#[test]
fn refused_shortcut_proposal_clears_only_its_own_intent() {
    let (provider, mut app) = demo();
    // A shortcut Apply whose enqueue is refused (empty expression stages
    // nothing) clears the intent its own invocation just created instead of
    // waiting for a request that never runs.
    app.handle(
        Action::ApplyAskProposal {
            kind: AskAiKind::Enrichment,
            expression: String::new(),
            recipe: None,
            outcome: None,
            task: Some(AskTask::SeverityColumn),
        },
        &provider,
    );
    assert!(app.take_query_requests().is_empty());
    assert!(
        app.view_state().unwrap().pending_role.is_none(),
        "a refused shortcut must not stay armed"
    );
}

#[test]
fn removed_stage_leaves_a_lingering_name_the_markers_refuse() {
    use lvu::TimeBasis;
    let (provider, mut app) = demo();
    // Accept a chain producing `event_time` through the real mutation path.
    app.handle(Action::Open(Open::Enrichment), &provider);
    alt(&mut app, &provider, KeyCode::Char('a'));
    paste(&mut app, &provider, "event_time = pl.col(\"ts\")");
    key(&mut app, &provider, KeyCode::Enter);
    let request = app.take_query_requests().pop().expect("chain query");
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: request.view_id.clone(),
        generation: request.generation,
        revision: request.revision,
        purpose: QueryPurpose::Enrichment,
        result: Ok(()),
    }));
    // Name the timestamp role and converge a Selected basis on it, as the
    // shortcut assignment does.
    {
        let state = app.views.active_mut().unwrap();
        state.timestamp_column = Some("event_time".into());
        state.applied_time_basis = TimeBasis::Selected;
        state.applied_time_field = Some(role_time_token("event_time"));
    }
    let state = app.view_state().unwrap();
    assert!(basis_covers_role(
        state.applied_time_basis,
        state.applied_time_field.as_deref(),
        state.timestamp_column.as_deref(),
    ));
    // Accept removal of the derived stage through the real editor flow while
    // the Selected basis still names the column (now reading raw).
    alt(&mut app, &provider, KeyCode::Char('e'));
    for _ in 0..8 {
        if app.layers.enrichment_step.control() == lvu::app::EnrichmentStepControl::Remove {
            break;
        }
        key(&mut app, &provider, KeyCode::Tab);
    }
    key(&mut app, &provider, KeyCode::Enter);
    let request = app
        .take_query_requests()
        .pop()
        .expect("a chain revalidation");
    assert!(
        request.constraints.enrichments.is_empty(),
        "the stage is really gone: {:?}",
        request.constraints.enrichments
    );
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: request.view_id.clone(),
        generation: request.generation,
        revision: request.revision,
        purpose: QueryPurpose::Enrichment,
        result: Ok(()),
    }));
    // The stored name lingers by design — and stays harmless because the
    // gutter resolves per row against the worker's markers, not the name: a
    // served row with no `derived.event_time` marker falls back to capture
    // time (see `event_stamp`), while a declared-but-unready row is a
    // placeholder. The worker half — no markers after removal — is proven in
    // `role_provenance`.
    let state = app.view_state().unwrap();
    assert_eq!(
        state.timestamp_column.as_deref(),
        Some("event_time"),
        "the stored name lingers; only row markers decide rendering"
    );
    assert!(state.enrichments.is_empty());
}

/// A slash-shorthand named capture assigned from Fields renders through
/// the real log frame, and its removal with a raw same-name field present
/// feeds nothing to the role.
///
/// The worker half — slash outputs published with structural markers, and
/// markers gone after removal — is proven through the real view path in
/// `role_provenance`. Here the rows mirror that worker output while every
/// decision (Fields assignment, chain accept/removal, frame render) runs
/// production code: Fields resolves through the `derived.` marker (never by
/// parsing definition source, so shorthand works), and the frame resolves
/// through `derived_ready.` (so the raw namesake cannot feed the role).
#[test]
fn slash_severity_assigns_from_fields_and_renders_until_removed() {
    use lvu::{DisplayRow, RowId, RowPage, RowProvider, SourceItem, ViewItem, ViewportRequest};
    use ratatui::{Terminal, backend::TestBackend};

    struct SlashRows {
        /// True while the accepted slash stage serves marked rows; false
        /// once it is removed and only the raw same-name field remains.
        marked: std::cell::Cell<bool>,
    }

    impl SlashRows {
        fn rows(&self) -> Vec<DisplayRow> {
            let marked = self.marked.get();
            // The raw same-name field deliberately carries canonical
            // uppercase text while the rendered event text stays lowercase:
            // only the structural markers — never the value — may decide the
            // role, so a marker-blind render would print WARN/ERROR in the
            // level cell and fail the removal assertion below.
            [(1u64, "WARN"), (2u64, "ERROR")]
                .into_iter()
                .map(|(sequence, canonical)| {
                    let details = if marked {
                        vec![
                            ("derived.severity".to_string(), canonical.to_string()),
                            ("derived_ready.severity".to_string(), canonical.to_string()),
                        ]
                    } else {
                        Vec::new()
                    };
                    DisplayRow {
                        id: RowId::new("s1", sequence),
                        timestamp: "07:34:42.627Z".into(),
                        captured_at_unix_nanos: Some(1_700_000_000_000_000_000),
                        level: String::new(),
                        text: format!(
                            "level={} request {sequence:02} completed",
                            canonical.to_lowercase(),
                        ),
                        details,
                        fields: vec![
                            ("level".to_string(), canonical.to_lowercase()),
                            ("severity".to_string(), canonical.to_string()),
                        ],
                    }
                })
                .collect()
        }
    }

    impl RowProvider for SlashRows {
        fn page(&self, _view_id: &str, request: ViewportRequest) -> RowPage {
            let rows = self.rows();
            let total = rows.len();
            let rows = rows
                .into_iter()
                .skip(request.start)
                .take(request.len.max(1))
                .collect();
            RowPage { total, rows }
        }

        fn row_by_id(&self, _view_id: &str, id: &RowId) -> Option<DisplayRow> {
            self.rows().into_iter().find(|row| &row.id == id)
        }

        fn index_of_id(&self, _view_id: &str, id: &RowId) -> Option<usize> {
            self.rows().iter().position(|row| &row.id == id)
        }

        fn revision(&self, _view_id: &str) -> u64 {
            u64::from(self.marked.get())
        }
    }

    fn screen(provider: &SlashRows, app: &mut App, width: u16, height: u16) -> String {
        let theme = lvu::theme::ThemeId::LoveDark.theme();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        terminal
            .draw(|frame| lvu::ui::render_with_theme(frame, app, provider, theme, None))
            .expect("render");
        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    let provider = SlashRows {
        marked: std::cell::Cell::new(true),
    };
    let sources = vec![SourceItem {
        id: "s1".into(),
        name: "slash fixture".into(),
        health: "synthetic/static".into(),
    }];
    let views = vec![ViewItem {
        id: "v1".into(),
        source_id: "s1".into(),
        name: "All events".into(),
    }];
    let mut app = App::new(sources, views, true);
    app.sync_provider(&provider, 10);

    // Accept the slash shorthand through the real editor flow: the
    // assignment-only name helper cannot see this output, which is exactly
    // the regression — rendering must not consult it either.
    app.handle(Action::Open(Open::Enrichment), &provider);
    alt(&mut app, &provider, KeyCode::Char('a'));
    paste(&mut app, &provider, "/level=(?P<severity>WARN|ERROR)/");
    key(&mut app, &provider, KeyCode::Enter);
    let request = app.take_query_requests().pop().expect("chain query");
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: request.view_id.clone(),
        generation: request.generation,
        revision: request.revision,
        purpose: QueryPurpose::Enrichment,
        result: Ok(()),
    }));
    key(&mut app, &provider, KeyCode::Esc);

    // Assign the role from Fields: `level` is raw (no marker, refused), `j`
    // reaches the marked `severity` output and `s` names it.
    app.sync_provider(&provider, 10);
    app.handle(Action::Open(Open::Fields), &provider);
    key(&mut app, &provider, KeyCode::Char('j'));
    key(&mut app, &provider, KeyCode::Char('s'));
    assert_eq!(
        app.view_state().unwrap().severity_column.as_deref(),
        Some("severity"),
        "Fields assigns a slash output through its marker: {:?}",
        app.action_notice
    );
    key(&mut app, &provider, KeyCode::Esc);

    // The canonical ready value populates the level cell through the real
    // frame — the assignment-only inventory would have suppressed it.
    let text = screen(&provider, &mut app, 120, 30);
    assert!(
        text.contains("WARN") && text.contains("ERROR"),
        "slash severity renders: {text}"
    );

    // Remove the stage through the real editor flow while the raw same-name
    // field stays: the lingering stored name must not feed the role.
    app.handle(Action::Open(Open::Enrichment), &provider);
    alt(&mut app, &provider, KeyCode::Char('e'));
    for _ in 0..8 {
        if app.layers.enrichment_step.control() == lvu::app::EnrichmentStepControl::Remove {
            break;
        }
        key(&mut app, &provider, KeyCode::Tab);
    }
    key(&mut app, &provider, KeyCode::Enter);
    let request = app
        .take_query_requests()
        .pop()
        .expect("a chain revalidation");
    assert!(request.constraints.enrichments.is_empty());
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: request.view_id.clone(),
        generation: request.generation,
        revision: request.revision,
        purpose: QueryPurpose::Enrichment,
        result: Ok(()),
    }));
    key(&mut app, &provider, KeyCode::Esc);
    provider.marked.set(false);
    app.sync_provider(&provider, 10);
    assert_eq!(
        app.view_state().unwrap().severity_column.as_deref(),
        Some("severity"),
        "the stored name lingers after removal"
    );
    let text = screen(&provider, &mut app, 120, 30);
    assert!(
        !text.contains("WARN") && !text.contains("ERROR"),
        "raw same-name values must not feed the removed role: {text}"
    );
    assert!(
        text.contains("level=warn"),
        "raw input itself stays visible: {text}"
    );
}

#[test]
fn new_tasks_carry_review_labels() {
    // The Ask dialog titles each task; two tasks sharing a title would be
    // indistinguishable where the user chooses.
    for task in [AskTask::SeverityColumn, AskTask::TimestampColumn] {
        assert!(!task.object().is_empty());
        assert!(!task.summary().is_empty());
        assert!(!task.message().is_empty());
    }
    assert_ne!(
        AskTask::SeverityColumn.object(),
        AskTask::TimestampColumn.object()
    );
}
