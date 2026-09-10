//! Acceptance for dataset-relative time ranges and gap navigation.
//!
//! Both features exist because "when" is the question a log reader actually
//! asks, and both were previously answerable only against the wall clock. What
//! is asserted here is what a user meets:
//!
//! * a window measured against the data, not the clock, and the rolling
//!   clock-relative window still behaving exactly as it did;
//! * `{` and `}` moving the selection to a quiet period, saying which one, and
//!   changing no constraint while they do it;
//! * the gap threshold visible and editable in the Time dialog rather than
//!   being a number the user has to know.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lvu::{
    Action, App, CaptureTimePolicy, GapDirection, RowId, RowProvider, ViewRole,
    app::{DEFAULT_AROUND_SECONDS, DEFAULT_GAP_THRESHOLD_SECONDS, TimeWindowChoice},
    component::{Open, RawEvent},
    components::time::TimeControl,
    fixture::FixtureProvider,
    theme::Theme,
    ui,
};
use ratatui::{Terminal, backend::TestBackend};

fn demo() -> (FixtureProvider, App) {
    let (provider, sources, views) = FixtureProvider::demo();
    (provider, App::new(sources, views, true))
}

/// Press a key the way the terminal delivers it: a converted layer takes raw
/// events, the base focus goes through the shell's key table.
fn key(app: &mut App, provider: &FixtureProvider, code: KeyCode) {
    let event = KeyEvent::new(code, KeyModifiers::NONE);
    if app.focus == lvu::Focus::Layer {
        app.handle(Action::Raw(RawEvent::Key(event)), provider);
    } else {
        let action = app.key_to_action(event);
        app.handle(action, provider);
    }
}

fn screen(provider: &FixtureProvider, app: &mut App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| ui::render_with_theme(frame, app, provider, Theme::TERMINAL, None))
        .unwrap();
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

/// Walk the Time dialog's focus ring to a control, the way a user tabs to it.
fn focus(app: &mut App, provider: &FixtureProvider, control: TimeControl) {
    for _ in 0..24 {
        if app.layers.time.state().focus == control {
            return;
        }
        key(app, provider, KeyCode::Tab);
    }
    panic!("{control:?} never took focus");
}

/// Open a dropdown and commit the row at `index`, the way a user arrows to it.
fn choose_row(
    app: &mut App,
    provider: &FixtureProvider,
    control: TimeControl,
    index: usize,
    len: usize,
) {
    focus(app, provider, control);
    key(app, provider, KeyCode::Enter);
    let selected = app.layers.time.state().highlighted;
    for _ in 0..(index + len - selected) % len {
        key(app, provider, KeyCode::Down);
    }
    key(app, provider, KeyCode::Enter);
}

/// Set the view's gap threshold through the dialog, which is the only way a
/// user can set it.
fn set_gap_threshold(app: &mut App, provider: &FixtureProvider, seconds: u64) {
    app.handle(Action::Open(Open::Time), provider);
    let index = GAP_CHOICES
        .iter()
        .position(|value| *value == seconds)
        .expect("an offered threshold");
    choose_row(app, provider, TimeControl::Gap, index, GAP_CHOICES.len());
    key(app, provider, KeyCode::Esc);
    assert_eq!(app.view_state().unwrap().gap_threshold_seconds(), seconds);
}

/// The thresholds the dialog offers, mirrored here so a test can name one.
const GAP_CHOICES: [u64; 7] = [1, 10, 30, 60, 300, 900, 3600];

/// A workspace whose stream has two real quiet periods in it.
fn gapped() -> (FixtureProvider, App) {
    let (provider, sources, views) = FixtureProvider::gapped();
    let mut app = App::new(sources, views, true);
    app.sync_provider(&provider, 8);
    (provider, app)
}

#[test]
fn the_dialog_offers_windows_measured_against_the_data_and_says_which_is_which() {
    let (provider, mut app) = demo();
    app.sync_provider(&provider, 8);
    app.handle(Action::Open(Open::Time), &provider);
    let rendered = screen(&provider, &mut app, 120, 40);

    // The clock-relative and data-relative windows are named apart. "Last 5m"
    // meaning two different things depending on a mode is exactly the
    // inference this row of the TODO is about removing.
    let choices = app.layers.time.state().window_choices.clone();
    let labels: Vec<String> = choices
        .iter()
        .map(|choice| lvu::app::time_window_label(*choice))
        .collect();
    assert!(
        labels.iter().any(|label| label == "Last 5m by clock"),
        "{labels:?}"
    );
    assert!(
        labels.iter().any(|label| label == "Last 5m of data"),
        "{labels:?}"
    );
    assert!(
        labels.iter().any(|label| label == "First → last event"),
        "{labels:?}"
    );
    assert!(
        labels.iter().any(|label| label == "± 30s around selected"),
        "the ± width is part of the choice, not a hidden constant: {labels:?}"
    );
    assert!(rendered.contains("Gap jump"), "{rendered}");
}

#[test]
fn a_data_relative_window_resolves_against_the_dataset_not_the_clock() {
    let (provider, mut app) = demo();
    app.sync_provider(&provider, 8);
    let bounds = provider
        .time_bounds("all", lvu::TimeBasis::Capture)
        .expect("fixture bounds");
    app.handle(Action::Open(Open::Time), &provider);

    // Pick "First → last event" from the window dropdown.
    focus(&mut app, &provider, TimeControl::Window);
    key(&mut app, &provider, KeyCode::Enter);
    let index = app
        .layers
        .time
        .state()
        .window_choices
        .iter()
        .position(|choice| *choice == TimeWindowChoice::DataFirstToLast)
        .expect("the data range is offered");
    let selected = app.layers.time.state().highlighted;
    for _ in 0..(index + app.layers.time.state().window_choices.len() - selected)
        % app.layers.time.state().window_choices.len()
    {
        key(&mut app, &provider, KeyCode::Down);
    }
    key(&mut app, &provider, KeyCode::Enter);

    // Choosing it fills the fields, so the user can see and narrow what it
    // means before applying anything.
    let state = app.view_state().unwrap();
    assert_eq!(
        state.time_start_draft,
        lvu::format_utc_nanos(bounds.first_unix_nanos)
    );
    assert_eq!(
        state.time_end_draft,
        lvu::format_utc_nanos(bounds.last_unix_nanos + 1),
        "half-open bounds put the last event inside the window it ends"
    );

    // Applying it stores an absolute policy: a data range is a way of picking
    // a window, not a second kind of rolling window.
    focus(&mut app, &provider, TimeControl::Apply);
    key(&mut app, &provider, KeyCode::Enter);
    let request = app.take_query_requests().pop().expect("one query");
    let window = request.constraints.capture_time.expect("a window");
    assert_eq!(window.start_unix_nanos, bounds.first_unix_nanos);
    assert_eq!(window.end_unix_nanos, bounds.last_unix_nanos + 1);
    assert!(app.apply_query_completion(lvu::QueryCompletion {
        view_id: request.view_id.clone(),
        generation: request.generation,
        revision: request.revision,
        purpose: request.purpose,
        result: Ok(()),
    }));
    assert!(
        matches!(
            app.view_state().unwrap().applied_capture_time_policy,
            Some(CaptureTimePolicy::Absolute(_))
        ),
        "a data range is a way of picking a window, not a second rolling policy"
    );
}

#[test]
fn the_rolling_clock_window_is_unchanged() {
    // The existing behaviour this row must preserve: `Recent` stays a policy,
    // resolved against the clock and re-resolved as the clock moves.
    let (provider, mut app) = demo();
    app.sync_provider(&provider, 8);
    app.handle(Action::Open(Open::Time), &provider);
    app.handle(
        Action::Raw(RawEvent::Key(KeyEvent::new(
            KeyCode::Char('5'),
            KeyModifiers::ALT,
        ))),
        &provider,
    );
    let request = app.take_query_requests().pop().expect("one query");
    assert!(app.apply_query_completion(lvu::QueryCompletion {
        view_id: request.view_id.clone(),
        generation: request.generation,
        revision: request.revision,
        purpose: request.purpose,
        result: Ok(()),
    }));
    assert!(matches!(
        app.view_state().unwrap().applied_capture_time_policy,
        Some(CaptureTimePolicy::Recent { seconds: 300 })
    ));
}

#[test]
fn gap_navigation_moves_the_selection_and_reports_what_it_found() {
    let (provider, mut app) = gapped();
    // Records at +0s, +1s, +2s, +602s, +603s, +2403s: gaps of 10m and 30m.
    app.handle(Action::Top, &provider);

    // The default threshold is a minute, so the first quiet period found is
    // the ten-minute one, and the jump lands on the record that resumes.
    key(&mut app, &provider, KeyCode::Char('}'));
    assert_eq!(
        app.view_state().unwrap().selected,
        Some(RowId::new("gaps", 4)),
        "the jump lands on the record that resumes, not on the last quiet one"
    );
    let notice = app
        .view_state()
        .unwrap()
        .gap_notice
        .clone()
        .expect("a report");
    assert!(notice.starts_with("gap 10m · quiet from "), "{notice}");

    // Navigation changes nothing about the view's definition.
    assert!(app.take_query_requests().is_empty());
    assert!(app.view_state().unwrap().search.applied.is_empty());
    assert_eq!(app.view_state().unwrap().applied_capture_time, None);
    assert!(
        !app.view_state().unwrap().follow,
        "a jump leaves follow mode"
    );

    // The status line carries the report where the user is already looking.
    let rendered = screen(&provider, &mut app, 120, 24);
    assert!(rendered.contains("gap 10m"), "{rendered}");

    // There is no earlier gap, so backward says so and stays put.
    let landed = app.view_state().unwrap().selected.clone();
    key(&mut app, &provider, KeyCode::Char('{'));
    assert_eq!(app.view_state().unwrap().selected, landed);
    assert_eq!(
        app.action_notice.as_deref(),
        Some("no gap longer than 1m before here")
    );
}

#[test]
fn a_repeated_jump_advances_instead_of_standing_still() {
    let (provider, mut app) = gapped();
    app.handle(Action::Top, &provider);

    key(&mut app, &provider, KeyCode::Char('}'));
    assert_eq!(
        app.view_state().unwrap().selected,
        Some(RowId::new("gaps", 4))
    );
    key(&mut app, &provider, KeyCode::Char('}'));
    assert_eq!(
        app.view_state().unwrap().selected,
        Some(RowId::new("gaps", 6)),
        "pressing the key again must reach the next gap"
    );
    // And backward retraces it.
    key(&mut app, &provider, KeyCode::Char('{'));
    assert_eq!(
        app.view_state().unwrap().selected,
        Some(RowId::new("gaps", 4))
    );
}

#[test]
fn the_gap_threshold_is_visible_and_editable_in_the_dialog() {
    let (provider, mut app) = gapped();
    app.handle(Action::Open(Open::Time), &provider);
    let rendered = screen(&provider, &mut app, 120, 40);
    assert!(
        rendered.contains(&format!(
            "Quiet ≥ {}",
            lvu::format_capture_duration(DEFAULT_GAP_THRESHOLD_SECONDS)
        )),
        "the threshold is stated, not assumed:\n{rendered}"
    );

    key(&mut app, &provider, KeyCode::Esc);

    // Raising it past the ten-minute gap makes the jump find the thirty-minute
    // one instead: the threshold is what the key acts on, not decoration.
    set_gap_threshold(&mut app, &provider, 900);
    app.handle(Action::Top, &provider);
    key(&mut app, &provider, KeyCode::Char('}'));
    assert_eq!(
        app.view_state().unwrap().selected,
        Some(RowId::new("gaps", 6))
    );
}

#[test]
fn the_around_window_uses_the_width_the_choice_names() {
    let (provider, mut app) = demo();
    app.sync_provider(&provider, 8);
    app.handle(Action::Top, &provider);
    let anchor = app.view_state().unwrap().selected.clone().unwrap();
    let anchor_time = provider
        .row_by_id("all", &anchor)
        .unwrap()
        .captured_at_unix_nanos
        .unwrap();
    app.handle(Action::Open(Open::Time), &provider);

    focus(&mut app, &provider, TimeControl::Window);
    key(&mut app, &provider, KeyCode::Enter);
    let target = TimeWindowChoice::AroundSelected(300);
    let choices = app.layers.time.state().window_choices.clone();
    let index = choices
        .iter()
        .position(|choice| *choice == target)
        .expect("a wider context is offered");
    let selected = app.layers.time.state().highlighted;
    for _ in 0..(index + choices.len() - selected) % choices.len() {
        key(&mut app, &provider, KeyCode::Down);
    }
    key(&mut app, &provider, KeyCode::Enter);

    // ± 5 minutes, not the ± 30 seconds the width used to be hard-coded at.
    let state = app.view_state().unwrap();
    assert_eq!(
        state.time_start_draft,
        lvu::format_utc_nanos(anchor_time - 300_000_000_000)
    );
    assert_eq!(
        state.time_end_draft,
        lvu::format_utc_nanos(anchor_time + 300_000_000_000)
    );
    assert_eq!(DEFAULT_AROUND_SECONDS, 30, "the default width is unchanged");
}

#[test]
fn a_view_that_cannot_report_bounds_is_not_offered_a_data_relative_window() {
    // Offering a choice that would silently do nothing is worse than not
    // offering it. `NoBounds` stands for a provider that cannot answer.
    struct NoBounds(FixtureProvider);
    impl RowProvider for NoBounds {
        fn page(&self, view_id: &str, request: lvu::ViewportRequest) -> lvu::RowPage {
            self.0.page(view_id, request)
        }
        fn row_by_id(&self, view_id: &str, id: &RowId) -> Option<lvu::DisplayRow> {
            self.0.row_by_id(view_id, id)
        }
        fn index_of_id(&self, view_id: &str, id: &RowId) -> Option<usize> {
            self.0.index_of_id(view_id, id)
        }
        fn revision(&self, view_id: &str) -> u64 {
            self.0.revision(view_id)
        }
    }
    let (fixture, sources, views) = FixtureProvider::demo();
    let provider = NoBounds(fixture);
    let mut app = App::new(sources, views, true);
    app.sync_provider(&provider, 8);
    app.handle(Action::Open(Open::Time), &provider);
    let choices = app.layers.time.state().window_choices.clone();
    assert!(
        !choices
            .iter()
            .any(|choice| matches!(choice, TimeWindowChoice::DataFirstToLast)),
        "{choices:?}"
    );
    assert!(
        choices.contains(&TimeWindowChoice::Recent(300)),
        "the clock-relative windows still work without bounds: {choices:?}"
    );

    // And the key still answers rather than doing nothing silently.
    app.handle(
        Action::Raw(RawEvent::Key(KeyEvent::new(
            KeyCode::Esc,
            KeyModifiers::NONE,
        ))),
        &provider,
    );
    app.handle(Action::JumpToGap(GapDirection::Forward), &provider);
    assert!(app.action_notice.is_some());
}

#[test]
fn gap_navigation_works_on_the_canonical_view_it_never_filters() {
    // All events is where gaps matter most, and it is the one view that is
    // never filtered in place. Navigation must work there and must not fork.
    let (provider, mut app) = demo();
    let canonical = app.active_view_id().unwrap().to_owned();
    app.set_view_role(&canonical, ViewRole::Canonical);
    app.sync_provider(&provider, 8);
    app.handle(Action::Top, &provider);
    // Fixture rows are one second apart, so nothing exceeds the threshold.
    key(&mut app, &provider, KeyCode::Char('}'));
    assert!(
        app.take_view_fork_requests().is_empty(),
        "navigation never forks"
    );
    assert!(app.take_query_requests().is_empty());
}

// ---- Explicit display and order ----------------------------------------
//
// The third half of the row: what a timestamp *means* was inferred twice
// over. The column was UTC without saying so, and the row order has always
// been arrival order whatever basis the view is filtered on.

#[test]
fn the_display_zone_reformats_the_column_and_says_which_zone_it_is() {
    let (provider, mut app) = demo();
    app.sync_provider(&provider, 8);
    // Fixture rows are one second apart from the epoch, so the first is
    // 00:00:01 UTC and its offset forms are exact.
    let utc = screen(&provider, &mut app, 120, 24);
    assert!(utc.contains("12:00:01.000Z"), "{utc}");
    assert!(utc.contains("tz:UTC"), "{utc}");

    app.appearance.display_zone = "+02:00".into();
    let shifted = screen(&provider, &mut app, 120, 24);
    assert!(
        shifted.contains("14:00:01.000+02:00"),
        "the same instant, in the chosen offset:\n{shifted}"
    );
    assert!(shifted.contains("tz:UTC+02:00"), "{shifted}");
    assert!(
        !shifted.contains("12:00:01.000Z"),
        "the column is reformatted, not annotated:\n{shifted}"
    );

    // A negative offset crosses midnight backwards without losing the suffix.
    app.appearance.display_zone = "-05:00".into();
    let west = screen(&provider, &mut app, 120, 24);
    assert!(west.contains("07:00:01.000-05:00"), "{west}");
}

#[test]
fn named_zone_uses_the_offset_at_each_instant_and_invalid_tokens_fall_back() {
    // A settings file written by a newer build must not make the log
    // unreadable. `lvu-app` refuses the value at load; if one reaches the
    // renderer anyway, UTC is the honest fallback.
    let winter = 1_768_478_400_000_000_000;
    let summer = 1_784_116_800_000_000_000;
    assert_eq!(
        lvu::app::format_display_time(winter, "Europe/Berlin"),
        "13:00:00.000+01:00"
    );
    assert_eq!(
        lvu::app::format_display_time(summer, "Europe/Berlin"),
        "14:00:00.000+02:00"
    );
    assert!(lvu::app::validate_display_zone("Europe/Berlin").is_ok());
    assert!(lvu::app::validate_display_zone("Europe/Not_A_Zone").is_err());
    assert_eq!(
        lvu::app::format_display_time(1_000_000_000, "Europe/Not_A_Zone"),
        lvu::app::format_display_time(1_000_000_000, "Z")
    );
    for (token, minutes) in [("Z", 0), ("+05:45", 345), ("-03:00", -180)] {
        assert_eq!(lvu::app::time_zone_offset_minutes(token), Some(minutes));
    }
}

#[test]
fn named_zone_date_formatting_crosses_local_midnight_at_the_same_instant() {
    let instant = 1_767_310_200_000_000_000; // 2026-01-01 23:30:00 UTC
    assert_eq!(
        lvu::app::format_display_date(instant, "Europe/Berlin"),
        "2026-01-02"
    );
    assert_eq!(
        lvu::app::format_display_date(instant, "America/New_York"),
        "2026-01-01"
    );
}

#[test]
fn the_time_dialog_states_the_zone_and_the_order_it_does_not_own() {
    let (provider, mut app) = demo();
    app.sync_provider(&provider, 8);
    app.appearance.display_zone = "+02:00".into();
    app.handle(Action::Open(Open::Time), &provider);
    let rendered = screen(&provider, &mut app, 120, 40);
    assert!(rendered.contains("Shown in UTC+02:00"), "{rendered}");
    assert!(
        rendered.contains("order: capture (arrival)"),
        "the row order is stated, not inferred from the basis:\n{rendered}"
    );
    assert!(
        rendered.contains("Settings"),
        "and it says where the zone is changed:\n{rendered}"
    );
}

#[test]
fn the_status_line_names_the_zone_and_leaves_the_order_to_the_dialog() {
    let (provider, mut app) = demo();
    app.sync_provider(&provider, 8);
    // The zone is on the line whatever it is: it is the one thing about a
    // timestamp the user chose, and this is the only place they see it without
    // opening anything.
    let utc = screen(&provider, &mut app, 120, 24);
    assert!(utc.contains("tz:UTC"), "{utc}");

    app.appearance.display_zone = "+05:45".into();
    let shifted = screen(&provider, &mut app, 120, 24);
    assert!(shifted.contains("tz:UTC+05:45"), "{shifted}");
    // The row order — the other half of what a timestamp means — is stated in
    // full on the Time dialog rather than here: it is the same for every view
    // and is not settable, and this line has a fixed width the search term and
    // the fold count are already competing for.
    assert!(!shifted.contains("order:capture"), "{shifted}");
}

#[test]
fn a_row_with_no_capture_time_keeps_what_the_provider_wrote() {
    // There is nothing to re-format from, so inventing a time would be worse
    // than showing the provider's own string.
    assert_eq!(
        lvu::app::display_time_width("Z"),
        13,
        "UTC's suffix is one character"
    );
    assert_eq!(
        lvu::app::display_time_width("+02:00"),
        18,
        "an offset's is six, and the column widens rather than truncating"
    );
    assert_eq!(
        lvu::app::display_time_width("Europe/Berlin"),
        18,
        "named zones always show their numeric offset at the instant"
    );
}

/// The order row names the order the rows are actually in.
///
/// A timestamp means nothing without the zone it is drawn in and the order the
/// rows are in; a merged view is only *fully* in its basis's order when every
/// source is, so a source that arrives out of order is named rather than
/// quietly sorted away (docs/merged-view-ordering.md I2).
#[test]
fn the_order_row_says_what_the_merge_did_and_what_it_refused_to_do() {
    struct Ordered(FixtureProvider, Option<lvu::provider::ViewOrder>);
    impl RowProvider for Ordered {
        fn page(&self, view_id: &str, request: lvu::ViewportRequest) -> lvu::RowPage {
            self.0.page(view_id, request)
        }
        fn row_by_id(&self, view_id: &str, id: &RowId) -> Option<lvu::DisplayRow> {
            self.0.row_by_id(view_id, id)
        }
        fn index_of_id(&self, view_id: &str, id: &RowId) -> Option<usize> {
            self.0.index_of_id(view_id, id)
        }
        fn revision(&self, view_id: &str) -> u64 {
            self.0.revision(view_id)
        }
        fn view_order(&self, _view_id: &str) -> Option<lvu::provider::ViewOrder> {
            self.1
        }
    }

    let row = |order: Option<lvu::provider::ViewOrder>| {
        let (fixture, sources, views) = FixtureProvider::demo();
        let provider = Ordered(fixture, order);
        let mut app = App::new(sources, views, true);
        app.sync_provider(&provider, 8);
        app.handle(Action::Open(Open::Time), &provider);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 34)).unwrap();
        terminal
            .draw(|frame| {
                lvu::ui::render_with_theme(frame, &mut app, &provider, Theme::TERMINAL, None)
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    // A provider that cannot say, and a single-source view: arrival, as before.
    assert!(row(None).contains("order: capture (arrival)"));
    assert!(
        row(Some(lvu::provider::ViewOrder {
            basis: lvu::TimeBasis::Event,
            sources: 1,
            out_of_order: 0,
            interleaved: true,
        }))
        .contains("order: recognized (arrival)")
    );

    // The capture basis concatenates, so the row says whose order it is: the
    // user arranged it in the View dialog and nothing overruled them.
    assert!(
        row(Some(lvu::provider::ViewOrder {
            basis: lvu::TimeBasis::Capture,
            sources: 3,
            out_of_order: 2,
            interleaved: false,
        }))
        .contains("order: capture · source order")
    );

    // Merged and fully in order.
    assert!(
        row(Some(lvu::provider::ViewOrder {
            basis: lvu::TimeBasis::Event,
            sources: 3,
            out_of_order: 0,
            interleaved: true,
        }))
        .contains("order: recognized · merged ·")
    );

    // Merged, but one source arrives out of order, so the claim is qualified.
    let qualified = row(Some(lvu::provider::ViewOrder {
        basis: lvu::TimeBasis::Event,
        sources: 3,
        out_of_order: 1,
        interleaved: true,
    }));
    assert!(
        qualified.contains("1 of 3 sources arrive out of order"),
        "{qualified}"
    );
}
