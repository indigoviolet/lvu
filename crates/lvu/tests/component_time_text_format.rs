//! Declaring a *text* enriched column as the event-time basis, from the
//! dialog's side of the seam.
//!
//! `lvu` cannot parse a chrono format — `lvu-live` and `lvu-query` depend on
//! it, not the other way round — so what is asserted here is that the dialog
//! never claims a rate it was not told, shows the format it was offered with
//! the evidence for it, lets the user replace it, and asks for the replacement
//! to be measured before it can be accepted.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lvu::{
    Action, App,
    app::{TimeFieldCandidate, TimeRecognition},
    component::{Open, RawEvent},
    fixture::FixtureProvider,
    theme::Theme,
    ui,
};
use ratatui::{Terminal, backend::TestBackend};

fn demo() -> (FixtureProvider, App) {
    let (provider, sources, views) = FixtureProvider::demo();
    (provider, App::new(sources, views, true))
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

fn key(app: &mut App, provider: &FixtureProvider, code: KeyCode) {
    app.handle(
        Action::Raw(RawEvent::Key(KeyEvent::new(code, KeyModifiers::NONE))),
        provider,
    );
}

/// What the host answers with for a text column: a reading whose format is
/// stated, whose coverage the query layer measured, and whose format is an
/// assumption the dialog has to have confirmed.
fn text_candidate(format: &str, coverage: u8) -> TimeFieldCandidate {
    TimeFieldCandidate {
        token: format!("column:started_at|text|reject|-|{format}"),
        label: "column: started_at".into(),
        reading: "text · RFC 3339".into(),
        coverage_percent: Some(coverage),
        text_format: Some(format.into()),
        assumptions: vec![
            format!("values are read with the format {format:?}"),
            "for example \"2026-09-07T12:00:01Z\"".into(),
        ],
        blocked: None,
        alternatives: Vec::new(),
    }
}

fn offer(app: &mut App, provider: &FixtureProvider, candidate: TimeFieldCandidate) {
    let request = app.layers.time.outbox.take().pop().unwrap();
    assert!(app.layers.time.complete_recognition(
        request.generation,
        TimeRecognition {
            sampled_records: 3,
            candidates: vec![candidate],
            ..Default::default()
        },
    ));
    // Choose it out of the basis dropdown: Enter opens, Down walks past the
    // three built-in bases, Enter picks.
    key(app, provider, KeyCode::Enter);
    for _ in 0..3 {
        key(app, provider, KeyCode::Down);
    }
    key(app, provider, KeyCode::Enter);
}

#[test]
fn a_text_column_shows_its_inferred_format_with_the_sample_and_the_rate() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Time), &provider);
    offer(&mut app, &provider, text_candidate("%+", 100));

    let text = screen(&provider, &mut app, 100, 30);
    assert!(text.contains("Format"), "{text}");
    assert!(text.contains("%+"), "{text}");
    assert!(
        text.contains("Coverage: 100% of 3 sampled records"),
        "{text}"
    );
    assert!(text.contains("2026-09-07T12:00:01Z"), "{text}");
    // The format is an assumption, so it cannot be applied unasked.
    assert!(text.contains("Accept assumption"), "{text}");
}

/// A rate below 100 is shown, not hidden, and does not block the basis: the
/// rows the format cannot read become nulls in this basis, and stay in the
/// view.
#[test]
fn a_partial_rate_is_shown_and_still_acceptable() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Time), &provider);
    offer(&mut app, &provider, text_candidate("%+", 67));

    let text = screen(&provider, &mut app, 100, 30);
    assert!(
        text.contains("Coverage: 67% of 3 sampled records"),
        "{text}"
    );
    assert!(text.contains("Accept assumption"), "{text}");
}

#[test]
fn editing_the_format_asks_for_it_to_be_measured_and_never_claims_a_rate() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Time), &provider);
    offer(&mut app, &provider, text_candidate("%+", 100));
    assert!(app.layers.time.outbox.take().is_empty(), "no probe yet");

    // Choosing the field leaves the focus on [ Accept assumption ]; the format
    // field is the control directly above it.
    key(&mut app, &provider, KeyCode::BackTab);
    key(&mut app, &provider, KeyCode::Backspace);
    key(&mut app, &provider, KeyCode::Backspace);
    for character in "%Y-%m-%d".chars() {
        key(&mut app, &provider, KeyCode::Char(character));
    }
    let text = screen(&provider, &mut app, 100, 30);
    assert!(text.contains("%Y-%m-%d"), "{text}");
    assert!(
        app.layers.time.outbox.take().is_empty(),
        "typing alone must not ask for anything"
    );

    key(&mut app, &provider, KeyCode::Enter);
    let probe = app
        .layers
        .time
        .outbox
        .take()
        .pop()
        .expect("Enter measures the format");
    let probe = probe.text_format_probe.expect("a format probe");
    assert_eq!(probe.column, "started_at");
    assert_eq!(probe.format, "%Y-%m-%d");
}

/// The measured answer replaces the reading it was an edit of, so what
/// `[ Accept assumption ]` accepts is always something that was measured.
#[test]
fn a_measured_edit_replaces_the_reading_it_was_an_edit_of() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Time), &provider);
    offer(&mut app, &provider, text_candidate("%+", 100));
    key(&mut app, &provider, KeyCode::BackTab);
    key(&mut app, &provider, KeyCode::Enter);
    let request = app.layers.time.outbox.take().pop().unwrap();

    app.layers.time.complete_recognition(
        request.generation,
        TimeRecognition {
            sampled_records: 3,
            probe: Some(TimeFieldCandidate {
                reading: "text · your format".into(),
                coverage_percent: Some(0),
                blocked: Some("this format reads none of the sampled values".into()),
                text_format: Some("%d/%m".into()),
                ..text_candidate("%d/%m", 0)
            }),
            ..Default::default()
        },
    );
    let text = screen(&provider, &mut app, 100, 30);
    assert!(text.contains("reads none of the sampled values"), "{text}");
    assert!(text.contains("%d/%m"), "{text}");
    // Nothing to accept: a format that reads nothing is not a basis.
    assert!(!text.contains("Accept assumption"), "{text}");
}

/// A format that could not survive the token that carries it is refused where
/// it is typed, without a round trip.
#[test]
fn a_format_containing_the_token_separator_is_refused_in_the_field() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Time), &provider);
    offer(&mut app, &provider, text_candidate("%+", 100));
    key(&mut app, &provider, KeyCode::BackTab);
    key(&mut app, &provider, KeyCode::Char('|'));
    key(&mut app, &provider, KeyCode::Enter);

    assert!(
        app.layers.time.outbox.take().is_empty(),
        "nothing is measured that cannot be carried"
    );
    let text = screen(&provider, &mut app, 100, 30);
    assert!(text.contains("cannot contain"), "{text}");
}
