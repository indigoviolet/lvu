//! Compatibility acceptance for the legacy Correlation component.
//!
//! New Fields correlation uses the shared-key Union chooser. Restored legacy
//! mappings still use this layer's request queue, completion fences, pending
//! lookup and per-source mapping, so those invariants remain covered here.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use lvu::{
    Action, App, CorrelationRequest, CorrelationSourceChoice, Focus,
    command_palette::CommandId,
    component::{Component, LayerId, Open, RawEvent},
    components::correlation::{CorrelationControl, CorrelationHit, CorrelationOpen},
    fixture::FixtureProvider,
    theme::Theme,
    ui,
};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

fn demo() -> (FixtureProvider, App) {
    let (provider, sources, views) = FixtureProvider::demo();
    let mut app = App::new(sources, views, true);
    app.sync_provider(&provider, 10);
    (provider, app)
}

fn draw(provider: &FixtureProvider, app: &mut App, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| ui::render_with_theme(frame, app, provider, Theme::TERMINAL, None))
        .unwrap();
    terminal.backend().buffer().clone()
}

fn screen(buffer: &Buffer) -> String {
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn key(app: &mut App, provider: &FixtureProvider, code: KeyCode) {
    app.handle(
        Action::Raw(RawEvent::Key(KeyEvent::new(code, KeyModifiers::NONE))),
        provider,
    );
}

fn click(app: &mut App, provider: &FixtureProvider, point: (u16, u16)) {
    app.handle(
        Action::Raw(RawEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: point.0,
            row: point.1,
            modifiers: KeyModifiers::NONE,
        })),
        provider,
    );
}

/// Open the legacy compatibility layer explicitly and return its fence.
/// Fields no longer routes new actions here; that replacement is pinned by
/// `fields_correlate_routes_to_the_shared_key_union` below.
fn start_legacy(app: &mut App, provider: &FixtureProvider) -> (u64, String) {
    app.handle(Action::Open(Open::Fields), provider);
    let row = lvu::components::fields::anchored_row(&app.views, provider)
        .expect("the fixture exposes its selected record")
        .id;
    key(app, provider, KeyCode::Esc);
    app.handle(
        Action::Open(Open::Correlation(CorrelationOpen {
            row,
            field: "service".into(),
        })),
        provider,
    );
    assert_eq!(app.layers.top(), Some(LayerId::Correlation));
    let generation = app.layers.correlation.generation();
    let origin = app.layers.correlation.origin_view_id().to_owned();
    (generation, origin)
}

fn choices() -> Vec<CorrelationSourceChoice> {
    vec![
        CorrelationSourceChoice {
            source_id: "api".into(),
            name: "API fixture".into(),
            fields: vec!["request_id".into(), "service".into()],
            chosen: Some("request_id".into()),
            incomplete: false,
        },
        CorrelationSourceChoice {
            source_id: "worker".into(),
            name: "Worker fixture".into(),
            // The same identity under a different key: nothing is pre-chosen.
            fields: vec!["req".into(), "stage".into()],
            chosen: None,
            incomplete: true,
        },
    ]
}

/// Start a lookup and answer it with the two-source mapping.
fn mapping(app: &mut App, provider: &FixtureProvider) -> (u64, String) {
    let (generation, origin) = start_legacy(app, provider);
    app.take_correlation_requests();
    assert!(app.open_correlation_dialog(
        generation,
        &origin,
        "request_id".into(),
        lvu_core::ExactScalar::string("req-7").unwrap(),
        "\"req-7\"".into(),
        choices(),
    ));
    assert!(!app.field_correlation_pending());
    (generation, origin)
}

fn accept_reason(app: &App) -> Option<&'static str> {
    app.layer_commands()
        .into_iter()
        .find(|(_, entry)| entry.spec.id == CommandId::CorrelationAccept)
        .map(|(_, entry)| entry.unavailable_reason)
        .expect("the Correlation layer contributes its verb")
}

#[test]
fn fields_correlate_routes_to_the_shared_key_union() {
    let (provider, mut app) = demo();
    let origin = app.active_view_id().unwrap().to_owned();
    app.handle(Action::Open(Open::Fields), &provider);
    let frozen = lvu::components::fields::anchored_row(&app.views, &provider)
        .unwrap()
        .id;

    key(&mut app, &provider, KeyCode::Char('r'));

    assert!(!app.layers.fields.is_open(), "Fields is replaced");
    assert_eq!(app.layers.top(), Some(LayerId::Union));
    assert!(
        app.take_correlation_requests().is_empty(),
        "new actions never enter the legacy raw-field mapping flow"
    );
    let chooser = screen(&draw(&provider, &mut app, 100, 30));
    assert!(chooser.contains("Union views"), "{chooser}");

    // The origin is preselected. Add the other fixture view and create so the
    // request proves the frozen identity and field passed to the app's native
    // accepted-derived authority check; no raw scalar is resolved in lvu.
    key(&mut app, &provider, KeyCode::Down);
    key(&mut app, &provider, KeyCode::Char(' '));
    let ready = screen(&draw(&provider, &mut app, 100, 30));
    assert!(ready.contains("2 of 2 views selected"), "{ready}");
    assert!(ready.contains("filtered by the"), "{ready}");
    key(&mut app, &provider, KeyCode::Enter);
    let requests = app.layers.union.take_requests();
    let [lvu::UnionDialogRequest::Create { inputs, shared_key }] = requests.as_slice() else {
        panic!("unexpected union requests: {requests:?}");
    };
    assert_eq!(inputs.len(), 2);
    let shared_key = shared_key.as_ref().expect("shared-key evidence");
    assert_eq!(shared_key.origin_view_id, origin);
    assert_eq!(shared_key.row_id, frozen);
    assert_eq!(shared_key.field, "service");
}

#[test]
fn the_lookup_names_the_frozen_record_and_a_stale_completion_is_dropped() {
    let (provider, mut app) = demo();
    let before = app
        .persistent_view_state(app.active_view_id().unwrap())
        .unwrap();
    let (generation, origin) = start_legacy(&mut app, &provider);
    let frozen = app
        .view_state()
        .unwrap()
        .field_picker_row
        .clone()
        .expect("Fields froze the selected record");
    assert!(
        !app.layers.fields.is_open(),
        "Fields is replaced, not stacked"
    );
    assert_eq!(app.focus, Focus::Layer);
    let requests = app.take_correlation_requests();
    match requests.as_slice() {
        [
            CorrelationRequest::Resolve {
                generation: queued,
                origin_view_id,
                row_id,
                field,
            },
        ] => {
            assert_eq!(*queued, generation);
            assert_eq!(origin_view_id, &origin);
            assert_eq!(row_id, &frozen);
            assert_eq!(field, "service");
        }
        other => panic!("unexpected requests: {other:?}"),
    }
    let pending = screen(&draw(&provider, &mut app, 100, 30));
    assert!(pending.contains("Correlate across sources"), "{pending}");
    assert!(
        pending.contains("finding records that share this value"),
        "{pending}"
    );
    assert!(
        pending.contains("service · from the selected record"),
        "{pending}"
    );
    assert!(pending.contains("[ Correlate ]"), "{pending}");
    assert!(pending.contains("[ Cancel ]"), "{pending}");

    assert!(app.is_correlation_current(generation, &origin));
    assert!(!app.finish_correlation(generation + 1, &origin, Ok("wrong".into())));
    assert!(app.is_correlation_current(generation, &origin));
    assert_eq!(
        app.persistent_view_state(&origin).unwrap().applied_search,
        before.applied_search
    );
    assert!(app.take_query_requests().is_empty());

    // A lookup that ends with a notice and no mapping closes the layer.
    assert!(app.finish_correlation(
        generation,
        &origin,
        Ok("Correlated service across 2 open sources".into())
    ));
    assert!(app.layers.stack.is_empty());
    assert_eq!(app.focus, Focus::Logs);
    assert_eq!(
        app.action_notice.as_deref(),
        Some("Correlated service across 2 open sources")
    );
}

#[test]
fn a_failed_lookup_stays_on_the_layer_as_its_error_state() {
    let (provider, mut app) = demo();
    let (generation, origin) = start_legacy(&mut app, &provider);
    app.take_correlation_requests();
    assert!(app.finish_correlation(generation, &origin, Err("journal closed".into())));
    assert_eq!(app.layers.top(), Some(LayerId::Correlation));
    assert!(!app.field_correlation_pending());
    let failed = screen(&draw(&provider, &mut app, 100, 30));
    assert!(
        failed.contains("correlation unavailable: journal closed"),
        "{failed}"
    );
    assert_eq!(
        accept_reason(&app),
        Some("the lookup failed; cancel and try again")
    );
    // Focus moved to Cancel, so Enter leaves. Nothing was queued.
    assert_eq!(app.layers.correlation.control(), CorrelationControl::Cancel);
    key(&mut app, &provider, KeyCode::Enter);
    assert!(app.layers.stack.is_empty());
    assert_eq!(app.focus, Focus::Logs);
    assert!(app.take_correlation_requests().is_empty());
}

#[test]
fn leaving_the_origin_view_cancels_the_lookup_and_fences_its_completion() {
    let (provider, mut app) = demo();
    let (generation, origin) = start_legacy(&mut app, &provider);
    app.take_correlation_requests();
    let other = app
        .views()
        .iter()
        .find(|view| view.id != origin)
        .map(|view| view.id.clone())
        .unwrap();
    app.select_view(&other);
    assert!(!app.is_correlation_current(generation, &origin));
    assert!(
        app.layers.stack.is_empty(),
        "the layer had nothing to wait for"
    );
    assert!(matches!(
        app.take_correlation_requests().as_slice(),
        [CorrelationRequest::Cancel { generation: cancelled, origin_view_id }]
            if *cancelled == generation && origin_view_id == &origin
    ));
    assert!(!app.finish_correlation(generation, &origin, Ok("stale".into())));
    assert_ne!(app.action_notice.as_deref(), Some("stale"));
}

#[test]
fn escape_withdraws_an_undrained_lookup_and_cancels_a_delivered_one() {
    let (provider, mut app) = demo();
    // Not yet handed to the adapter: the Resolve is simply withdrawn.
    let (generation, origin) = start_legacy(&mut app, &provider);
    key(&mut app, &provider, KeyCode::Esc);
    assert!(app.take_correlation_requests().is_empty());
    assert!(app.layers.stack.is_empty());
    assert!(!app.finish_correlation(generation, &origin, Ok("late".into())));

    // Delivered: a Cancel follows, and the generation stays counted until the
    // adapter answers, at which point the stale answer only releases it.
    for _ in 0..9 {
        let (generation, origin) = start_legacy(&mut app, &provider);
        assert!(matches!(
            app.take_correlation_requests().as_slice(),
            [CorrelationRequest::Resolve { .. }]
        ));
        assert!(!app.finish_correlation(generation, "wrong-view", Ok("wrong".into())));
        assert!(app.is_correlation_current(generation, &origin));
        key(&mut app, &provider, KeyCode::Esc);
        assert!(!app.is_correlation_current(generation, &origin));
        assert_eq!(app.focus, Focus::Logs);
        assert!(matches!(
            app.take_correlation_requests().as_slice(),
            [CorrelationRequest::Cancel { generation: cancelled, origin_view_id }]
                if *cancelled == generation && origin_view_id == &origin
        ));
        assert!(!app.finish_correlation(generation, &origin, Ok("stale".into())));
        assert_ne!(app.action_notice.as_deref(), Some("stale"));
    }
}

#[test]
fn capacity_is_bounded_and_a_full_queue_is_said_on_the_layer() {
    let (provider, mut app) = demo();
    for _ in 0..8 {
        start_legacy(&mut app, &provider);
        assert!(matches!(
            app.take_correlation_requests().as_slice(),
            [CorrelationRequest::Resolve { .. }]
        ));
        key(&mut app, &provider, KeyCode::Esc);
        // Cancelled but never answered: the adapter still holds it.
        app.take_correlation_requests();
    }
    start_legacy(&mut app, &provider);
    assert_eq!(app.layers.top(), Some(LayerId::Correlation));
    assert!(app.take_correlation_requests().is_empty());
    assert!(!app.field_correlation_pending());
    let full = screen(&draw(&provider, &mut app, 100, 30));
    assert!(
        full.contains("correlation request queue is full; try again shortly"),
        "{full}"
    );
    key(&mut app, &provider, KeyCode::Esc);
    assert!(app.layers.stack.is_empty());
}

#[test]
fn the_reserved_rows_keep_the_frame_where_it_was_when_the_lookup_answers() {
    let (provider, mut app) = demo();
    let (generation, origin) = start_legacy(&mut app, &provider);
    app.take_correlation_requests();
    let pending_screen = screen(&draw(&provider, &mut app, 100, 30));
    let pending = app.layers.correlation.surface();
    assert!(pending.popup.height > 0, "{pending_screen}");
    assert!(
        pending_screen.contains("finding records that share this value"),
        "{pending_screen}"
    );
    assert!(app.open_correlation_dialog(
        generation,
        &origin,
        "request_id".into(),
        lvu_core::ExactScalar::string("req-7").unwrap(),
        "\"req-7\"".into(),
        choices(),
    ));
    let answered = screen(&draw(&provider, &mut app, 100, 30));
    // §5.2.1: the list is a live region, so the popup rect does not move.
    assert_eq!(
        app.layers.correlation.surface().popup,
        pending.popup,
        "{answered}"
    );
    assert!(answered.contains("request_id = \"req-7\""), "{answered}");
    assert!(answered.contains("1 of 2"), "{answered}");
    assert!(answered.contains("Not correlated"), "{answered}");
}

#[test]
fn a_correlation_never_maps_a_differently_named_field_without_the_user_choosing_it() {
    let (provider, mut app) = demo();
    let (generation, _) = mapping(&mut app, &provider);
    let state = app.layers.correlation.mapping().unwrap().clone();
    assert_eq!(state.mapped_sources(), 1);
    // `worker` carries `req`, not `request_id`. Accepting now correlates only
    // the source whose field name actually matched.
    let correlation = state.correlation("request_id").unwrap();
    assert_eq!(correlation.source_ids().collect::<Vec<_>>(), vec!["api"]);
    assert_eq!(correlation.constraint_for("worker"), None);

    // Choosing `req` for the worker is an explicit act: select the row, open
    // its options, move to `req`, commit.
    key(&mut app, &provider, KeyCode::Down);
    key(&mut app, &provider, KeyCode::Enter);
    assert_eq!(app.layers.correlation.mapping().unwrap().popup, Some(0));
    key(&mut app, &provider, KeyCode::Down);
    key(&mut app, &provider, KeyCode::Enter);
    let correlation = app
        .layers
        .correlation
        .mapping()
        .unwrap()
        .correlation("request_id")
        .unwrap();
    assert_eq!(
        correlation
            .constraint_for("worker")
            .map(|c| c.field().to_owned()),
        Some("req".to_owned())
    );
    assert_eq!(correlation.fields(), vec!["req", "request_id"]);
    assert_eq!(accept_reason(&app), None);

    key(&mut app, &provider, KeyCode::Tab);
    assert_eq!(
        app.layers.correlation.control(),
        CorrelationControl::Correlate
    );
    key(&mut app, &provider, KeyCode::Enter);
    match app.take_correlation_requests().pop().unwrap() {
        CorrelationRequest::Accept {
            generation: accepted,
            correlation,
            name,
            ..
        } => {
            assert_eq!(accepted, generation);
            assert_eq!(name, "request_id = \"req-7\"");
            assert_eq!(
                correlation.source_ids().collect::<Vec<_>>(),
                vec!["api", "worker"]
            );
        }
        request => panic!("unexpected request: {request:?}"),
    }
    assert!(app.layers.correlation.mapping().unwrap().submitting);
    assert_eq!(accept_reason(&app), Some("the correlated view is opening"));
    assert!(app.correlation_accepted(generation, "Correlated 2 sources".into()));
    assert!(app.layers.stack.is_empty());
    assert_eq!(app.focus, Focus::Logs);
    assert_eq!(app.action_notice.as_deref(), Some("Correlated 2 sources"));
}

#[test]
fn a_rejected_or_cancelled_mapping_leaves_the_origin_view_exactly_as_it_was() {
    let (provider, mut app) = demo();
    let origin = app.active_view_id().unwrap().to_owned();
    let before = app.persistent_view_state(&origin).unwrap();
    let (generation, _) = mapping(&mut app, &provider);

    // The controller refuses the mapping: the dialog keeps the whole mapping
    // and says why. Nothing about the origin view moved.
    key(&mut app, &provider, KeyCode::Tab);
    key(&mut app, &provider, KeyCode::Enter);
    app.take_correlation_requests();
    assert!(app.correlation_accept_failed(generation, "a mapped source is no longer open".into()));
    let state = app.layers.correlation.mapping().unwrap().clone();
    assert!(!state.submitting);
    assert_eq!(state.mapped_sources(), 1);
    assert_eq!(
        app.layers.correlation.error(),
        Some("a mapped source is no longer open")
    );
    assert_eq!(app.layers.top(), Some(LayerId::Correlation));

    // Escape closes the popup first, then the layer. Neither touches the view.
    key(&mut app, &provider, KeyCode::BackTab);
    assert_eq!(
        app.layers.correlation.control(),
        CorrelationControl::Sources
    );
    key(&mut app, &provider, KeyCode::Enter);
    assert!(app.layers.correlation.mapping().unwrap().popup.is_some());
    key(&mut app, &provider, KeyCode::Esc);
    assert!(app.layers.correlation.mapping().unwrap().popup.is_none());
    assert_eq!(app.layers.top(), Some(LayerId::Correlation));
    key(&mut app, &provider, KeyCode::Esc);
    assert!(app.layers.stack.is_empty());
    assert_eq!(app.focus, Focus::Logs);

    assert_eq!(app.active_view_id(), Some(origin.as_str()));
    let after = app.persistent_view_state(&origin).unwrap();
    assert_eq!(after.applied_search, before.applied_search);
    assert_eq!(after.exact_field, before.exact_field);
    assert_eq!(after.source_ids, before.source_ids);
    assert!(app.take_query_requests().is_empty());
    assert!(app.take_correlation_requests().is_empty());
}

#[test]
fn an_empty_mapping_is_refused_and_every_rect_answers_the_hit_test() {
    let (provider, mut app) = demo();
    mapping(&mut app, &provider);
    // Unmap the one source that matched by name.
    key(&mut app, &provider, KeyCode::Enter);
    key(&mut app, &provider, KeyCode::Up);
    key(&mut app, &provider, KeyCode::Enter);
    assert_eq!(
        app.layers.correlation.mapping().unwrap().mapped_sources(),
        0
    );
    assert_eq!(accept_reason(&app), Some("map at least one source first"));

    key(&mut app, &provider, KeyCode::Tab);
    key(&mut app, &provider, KeyCode::Enter);
    assert!(app.take_correlation_requests().is_empty());
    assert_eq!(app.layers.top(), Some(LayerId::Correlation));
    let refused = screen(&draw(&provider, &mut app, 90, 24));
    assert!(refused.contains("[ Correlate ]"), "{refused}");
    assert!(refused.contains("request_id = \"req-7\""), "{refused}");
    assert!(refused.contains("Not correlated"), "{refused}");
    assert!(
        refused.contains("choose the field that carries this value in at"),
        "{refused}"
    );

    // Every button and row answers the hit test with itself, and a click on a
    // row opens its popup, whose choices then hit-test too.
    let surface = app.layers.correlation.surface();
    let mut controls = 0;
    let mut rows = 0;
    for y in surface.interior.y..surface.interior.bottom() {
        for x in surface.interior.x..surface.interior.right() {
            match app.layers.correlation.hit((x, y)) {
                Some(CorrelationHit::Control(_)) => controls += 1,
                Some(CorrelationHit::Row(_)) => rows += 1,
                Some(CorrelationHit::Choice(_)) => panic!("no popup is open"),
                None => {}
            }
        }
    }
    assert!(controls > 0 && rows > 0, "{controls} {rows}");
    let row = (0..surface.interior.bottom())
        .flat_map(|y| (0..surface.interior.right()).map(move |x| (x, y)))
        .find(|point| {
            matches!(
                app.layers.correlation.hit(*point),
                Some(CorrelationHit::Row(1))
            )
        })
        .unwrap();
    click(&mut app, &provider, row);
    assert_eq!(app.layers.correlation.mapping().unwrap().selected, 1);
    assert!(app.layers.correlation.mapping().unwrap().popup.is_some());
    draw(&provider, &mut app, 90, 24);
    let choice = (0..24)
        .flat_map(|y| (0..90).map(move |x| (x, y)))
        .find(|point| {
            matches!(
                app.layers.correlation.hit(*point),
                Some(CorrelationHit::Choice(1))
            )
        })
        .expect("the popup's rows are hit-testable");
    click(&mut app, &provider, choice);
    let state = app.layers.correlation.mapping().unwrap();
    assert!(state.popup.is_none());
    assert_eq!(state.sources[1].chosen.as_deref(), Some("req"));
}

#[test]
fn enter_runs_the_default_except_where_a_control_consumes_it() {
    let (provider, mut app) = demo();
    // While the lookup runs there is no mapping: Enter reaches the default,
    // which has nothing to accept, and nothing is queued.
    start_legacy(&mut app, &provider);
    app.take_correlation_requests();
    key(&mut app, &provider, KeyCode::Enter);
    assert!(app.take_correlation_requests().is_empty());
    assert_eq!(app.layers.top(), Some(LayerId::Correlation));
    assert_eq!(accept_reason(&app), Some("wait for the lookup to finish"));
    key(&mut app, &provider, KeyCode::Esc);

    // With a mapping the list consumes Enter (it opens the popup, §8.9), and
    // the palette row runs the default from anywhere.
    let (generation, _) = mapping(&mut app, &provider);
    key(&mut app, &provider, KeyCode::Enter);
    assert!(app.layers.correlation.mapping().unwrap().popup.is_some());
    key(&mut app, &provider, KeyCode::Esc);
    app.handle(
        Action::Command(LayerId::Correlation, CommandId::CorrelationAccept),
        &provider,
    );
    assert!(matches!(
        app.take_correlation_requests().as_slice(),
        [CorrelationRequest::Accept { generation: accepted, .. }] if *accepted == generation
    ));
}

#[test]
fn the_palette_rows_are_listed_from_the_base_and_say_why() {
    let (_provider, app) = demo();
    assert_eq!(
        accept_reason(&app),
        Some("open Fields and choose Correlate first")
    );
    let cancel = app
        .layer_commands()
        .into_iter()
        .find(|(id, entry)| {
            *id == LayerId::Correlation && entry.spec.id == CommandId::CorrelationCancel
        })
        .unwrap()
        .1;
    assert_eq!(
        cancel.unavailable_reason,
        Some("open Fields and choose Correlate first")
    );
}

#[test]
fn the_underlined_letters_press_the_buttons_through_the_shells_resolver() {
    let (provider, mut app) = demo();
    let (generation, _) = mapping(&mut app, &provider);
    // No text field ever has focus here, so the bare letters are the keys
    // (§8.10), and the focus ring stays on the list where the user was.
    key(&mut app, &provider, KeyCode::Char('r'));
    assert!(matches!(
        app.take_correlation_requests().as_slice(),
        [CorrelationRequest::Accept { generation: accepted, .. }] if *accepted == generation
    ));
    assert_eq!(
        app.layers.correlation.control(),
        CorrelationControl::Sources
    );
    // While the view opens the row is inert: `c` neither cancels nor closes.
    key(&mut app, &provider, KeyCode::Char('c'));
    assert_eq!(app.layers.top(), Some(LayerId::Correlation));
    assert!(app.correlation_accept_failed(generation, "refused".into()));
    key(&mut app, &provider, KeyCode::Char('C'));
    assert!(app.layers.stack.is_empty());
    assert_eq!(app.focus, Focus::Logs);
}

#[test]
fn responsive_frame_is_policy_stable_for_inspector_pending_and_mapping() {
    use lvu::dialog_layout::{ContextFootprint, PresentationKind, policy_size};

    // 80x24 and 54x16 share one Contextual Inspector frame per size; the
    // pending lookup and the answered mapping share it. The source list
    // scrolls via the shared plan; paint/mouse/scrollbar share its rects.
    for (width, height) in [(80u16, 24u16), (54, 16)] {
        let (provider, mut app) = demo();
        let (generation, origin) = start_legacy(&mut app, &provider);
        app.take_correlation_requests();
        let pending_text = screen(&draw(&provider, &mut app, width, height));
        assert!(
            pending_text.contains("Correlate across sources"),
            "{pending_text}"
        );
        assert!(pending_text.contains("finding records"), "{pending_text}");
        let pending = app.layers.correlation.surface().popup;
        let (want_w, want_h) = policy_size(
            ratatui::layout::Rect::new(0, 0, width, height),
            PresentationKind::Contextual(ContextFootprint::Inspector),
        );
        assert_eq!(
            (pending.width, pending.height),
            (want_w, want_h),
            "{width}x{height} frame must be policy"
        );
        assert!(app.open_correlation_dialog(
            generation,
            &origin,
            "request_id".into(),
            lvu_core::ExactScalar::string("req-7").unwrap(),
            "\"req-7\"".into(),
            choices(),
        ));
        let answered_text = screen(&draw(&provider, &mut app, width, height));
        assert_eq!(
            app.layers.correlation.surface().popup,
            pending,
            "{width}x{height} frame must not move when the lookup answers"
        );
        assert!(
            answered_text.contains("request_id = \"req-7\""),
            "{answered_text}"
        );
        assert!(answered_text.contains("Not correlated"), "{answered_text}");
        assert!(answered_text.contains("[ Correlate ]"), "{answered_text}");
        assert!(answered_text.contains("[ Cancel ]"), "{answered_text}");
    }

    // Below the floor the tiny fallback owns the frame.
    let (provider, mut app) = demo();
    let (_generation, _origin) = start_legacy(&mut app, &provider);
    let tiny = screen(&draw(&provider, &mut app, 19, 5));
    assert!(tiny.contains("terminal too small"), "{tiny}");
}

#[test]
fn source_list_scroll_reveals_with_matching_mouse() {
    // Twelve sources overflow the Inspector body at 80x24; Down scrolls the
    // shared viewport, the highlighted row stays selected, and a click opens
    // its field popup (same rects for paint and mouse).
    let (provider, mut app) = demo();
    let (generation, origin) = start_legacy(&mut app, &provider);
    app.take_correlation_requests();
    let many: Vec<CorrelationSourceChoice> = (0..12)
        .map(|index| CorrelationSourceChoice {
            source_id: format!("source-{index}"),
            name: format!("Source {index}"),
            fields: vec![format!("field-{index}")],
            chosen: if index == 0 {
                Some(format!("field-{index}"))
            } else {
                None
            },
            incomplete: false,
        })
        .collect();
    assert!(app.open_correlation_dialog(
        generation,
        &origin,
        "request_id".into(),
        lvu_core::ExactScalar::string("req-7").unwrap(),
        "\"req-7\"".into(),
        many,
    ));
    let buffer = draw(&provider, &mut app, 80, 24);
    assert!(screen(&buffer).contains("Correlate across sources"));
    for _ in 0..11 {
        key(&mut app, &provider, KeyCode::Down);
    }
    let scrolled = draw(&provider, &mut app, 80, 24);
    let scrolled_text = screen(&scrolled);
    assert!(scrolled_text.contains("Source 11"), "{scrolled_text}");
    assert!(
        !scrolled_text.contains("Source 0"),
        "viewport did not scroll: {scrolled_text}"
    );
    // Clicking the visible row opens its popup (same rects for paint/mouse).
    let surface = app.layers.correlation.surface();
    assert!(!surface.popup.is_empty());
    let row = (0..surface.interior.bottom())
        .flat_map(|y| (0..surface.interior.right()).map(move |x| (x, y)))
        .find(|point| {
            matches!(
                app.layers.correlation.hit(*point),
                Some(CorrelationHit::Row(11))
            )
        })
        .expect("scrolled row is hit-testable");
    click(&mut app, &provider, row);
    assert_eq!(app.layers.correlation.mapping().unwrap().selected, 11);
    assert!(app.layers.correlation.mapping().unwrap().popup.is_some());
}

#[test]
fn popup_uses_shared_anchored_geometry_and_shows_real_overflow() {
    // One source with 20 observed fields overflows the 8-row anchored popup;
    // only the shared viewport window is painted, and a click commits it.
    let (provider, mut app) = demo();
    let (generation, origin) = start_legacy(&mut app, &provider);
    app.take_correlation_requests();
    let fields: Vec<String> = (0..19).map(|index| format!("field-{index:02}")).collect();
    assert!(app.open_correlation_dialog(
        generation,
        &origin,
        "request_id".into(),
        lvu_core::ExactScalar::string("req-7").unwrap(),
        "\"req-7\"".into(),
        vec![
            CorrelationSourceChoice {
                source_id: "api".into(),
                name: "API fixture".into(),
                fields: fields.clone(),
                chosen: Some(fields[0].clone()),
                incomplete: false,
            },
            CorrelationSourceChoice {
                source_id: "worker".into(),
                name: "Worker fixture".into(),
                fields: vec!["req".into()],
                chosen: None,
                incomplete: false,
            },
        ],
    ));
    draw(&provider, &mut app, 80, 24);
    // Open the popup for the first source.
    key(&mut app, &provider, KeyCode::Enter);
    assert!(app.layers.correlation.mapping().unwrap().popup.is_some());
    let buffer = draw(&provider, &mut app, 80, 24);
    let rendered = screen(&buffer);
    assert!(rendered.contains("Not correlated"), "{rendered}");
    // Real overflow: the 8-row viewport shows the head, not the tail.
    assert!(rendered.contains("field-00"), "{rendered}");
    assert!(
        !rendered.contains("field-18"),
        "anchored popup did not window: {rendered}"
    );
    // Clicking a visible choice commits it (same rects for paint/mouse).
    let choice = (0..24)
        .flat_map(|y| (0..80).map(move |x| (x, y)))
        .find(|point| {
            matches!(
                app.layers.correlation.hit(*point),
                Some(CorrelationHit::Choice(1))
            )
        })
        .expect("popup choices are hit-testable");
    click(&mut app, &provider, choice);
    let state = app.layers.correlation.mapping().unwrap();
    assert!(state.popup.is_none());
    assert_eq!(state.sources[0].chosen.as_deref(), Some("field-00"));
}
