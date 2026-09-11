//! Acceptance for predicate colour rules as a component, and for the rendering
//! they drive.
//!
//! The two product claims under test: a rule is presentation, so an edit can
//! never narrow or destroy the applied view; and the terminal never decides
//! *whether* a rule matched — it only paints what the query engine reported and
//! re-locates the pattern inside the line it draws.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use lvu::{
    Action, App, ColorRule, DisplayRow, MAX_COLOR_RULES, QueryCompletion, QueryPurpose, RowId,
    RowPage, RowProvider, RuleColor, ViewportRequest,
    app::Focus,
    component::{Component, LayerId, Open, RawEvent},
    components::color_rules::{ColorRulesControl, ColorRulesHit},
    fixture::FixtureProvider,
    theme::Theme,
    ui,
};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, style::Modifier};

fn demo() -> (FixtureProvider, App) {
    let (provider, sources, views) = FixtureProvider::demo();
    (provider, App::new(sources, views, true))
}

fn draw<P: RowProvider>(provider: &P, app: &mut App, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| ui::render_with_theme(frame, app, provider, Theme::TERMINAL, None))
        .unwrap();
    terminal.backend().buffer().clone()
}

/// The row as one symbol per column, so an index into it *is* a column. A byte
/// offset from `str::find` is not: the box-drawing sidebar and every U+FFFD are
/// multi-byte, which is exactly the drift these tests exist to catch.
fn columns(buffer: &Buffer, row: u16) -> Vec<String> {
    (0..buffer.area.width)
        .map(|x| buffer[(x, row)].symbol().to_owned())
        .collect()
}

/// The first column at which `needle` starts on `row`.
fn column_of(buffer: &Buffer, row: u16, needle: &str) -> u16 {
    let cells = columns(buffer, row);
    let wanted: Vec<String> = needle.chars().map(|c| c.to_string()).collect();
    (0..cells.len())
        .find(|start| cells[*start..].starts_with(&wanted))
        .map(|start| start as u16)
        .unwrap_or_else(|| panic!("{needle:?} is not on row {row}"))
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

fn key(app: &mut App, provider: &impl RowProvider, code: KeyCode) {
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

fn type_text(app: &mut App, provider: &impl RowProvider, text: &str) {
    for character in text.chars() {
        key(app, provider, KeyCode::Char(character));
    }
}

/// Rows carrying the `color_rule` presentation metadata `lvu-view` attaches.
struct Painted(Vec<DisplayRow>);

impl RowProvider for Painted {
    fn page(&self, _: &str, request: ViewportRequest) -> RowPage {
        RowPage {
            total: self.0.len(),
            rows: self
                .0
                .iter()
                .skip(request.start)
                .take(request.len)
                .cloned()
                .collect(),
        }
    }
    fn row_by_id(&self, _: &str, id: &RowId) -> Option<DisplayRow> {
        self.0.iter().find(|row| &row.id == id).cloned()
    }
    fn index_of_id(&self, _: &str, id: &RowId) -> Option<usize> {
        self.0.iter().position(|row| &row.id == id)
    }
    fn revision(&self, _: &str) -> u64 {
        1
    }
}

fn painted_row(sequence: u64, text: &str, rule: Option<u16>) -> DisplayRow {
    let mut details = vec![("raw".to_owned(), text.to_owned())];
    if let Some(rule) = rule {
        details.push(("color_rule".to_owned(), rule.to_string()));
    }
    DisplayRow {
        id: RowId::new("api", sequence),
        timestamp: "12:00:00".into(),
        captured_at_unix_nanos: Some(1),
        level: "INFO".into(),
        text: text.to_owned(),
        details,
        fields: Vec::new(),
    }
}

#[test]
fn a_rule_is_added_edited_and_applied_without_narrowing_the_view() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::ColorRules), &provider);
    assert_eq!(app.focus, Focus::Layer);
    assert_eq!(app.layers.stack, vec![LayerId::ColorRules]);
    let opened = screen(&draw(&provider, &mut app, 90, 24));
    assert!(opened.contains("Colour rules"), "{opened}");
    assert!(opened.contains("no rules yet"), "{opened}");

    key(&mut app, &provider, KeyCode::Tab);
    while app.layers.color_rules.control() != ColorRulesControl::Add {
        key(&mut app, &provider, KeyCode::Tab);
    }
    key(&mut app, &provider, KeyCode::Enter);
    assert_eq!(
        app.layers.color_rules.control(),
        ColorRulesControl::Predicate,
        "adding a rule leaves the caret in its predicate"
    );
    type_text(&mut app, &provider, "timeout");
    assert_eq!(
        app.view_state().unwrap().color_rules_draft[0].predicate,
        "timeout"
    );
    // Editing the draft applies nothing: the accepted list, and so the rows on
    // screen, are untouched until Apply.
    assert!(app.view_state().unwrap().color_rules.is_empty());
    assert!(app.take_query_requests().is_empty());

    key(&mut app, &provider, KeyCode::Enter);
    let request = app.take_query_requests().pop().expect("one repaint query");
    assert_eq!(
        request.constraints.color_rules,
        vec![ColorRule {
            predicate: "timeout".into(),
            color: RuleColor::Red,
            column: None,
            value: None,
        }],
        "the rules travel to the engine as a constraint"
    );
    assert_eq!(
        request.constraints.text, request.base_constraints.text,
        "a repaint does not change what the view matches"
    );
    // Accepted only when the query that evaluated them lands, so the dialog's
    // "edited" state clears exactly when the rows repaint.
    assert!(
        app.view_state().unwrap().color_rules.is_empty(),
        "rules are not accepted before the repaint returns"
    );
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: request.view_id,
        generation: request.generation,
        revision: request.revision,
        purpose: request.purpose,
        result: Ok(()),
    }));
    assert_eq!(app.view_state().unwrap().color_rules.len(), 1);
    let settled = screen(&draw(&provider, &mut app, 90, 24));
    assert!(settled.contains("1 rule painting this view"), "{settled}");
}

#[test]
fn an_invalid_predicate_is_refused_where_it_was_typed() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::ColorRules), &provider);
    draw(&provider, &mut app, 90, 24);
    app.handle(
        Action::Raw(RawEvent::Key(KeyEvent::new(
            KeyCode::Char('a'),
            KeyModifiers::ALT,
        ))),
        &provider,
    );
    type_text(&mut app, &provider, "/(/");
    key(&mut app, &provider, KeyCode::Enter);
    let error = app.view_state().unwrap().color_rules_error.clone();
    assert!(
        error
            .as_deref()
            .is_some_and(|e| e.contains("invalid regex")),
        "{error:?}"
    );
    assert!(
        app.take_query_requests().is_empty(),
        "a rule that cannot compile never reaches the engine"
    );
    assert!(app.view_state().unwrap().color_rules.is_empty());
    assert!(app.layers.color_rules.is_open(), "the dialog stays open");
    let rendered = screen(&draw(&provider, &mut app, 90, 24));
    assert!(rendered.contains("invalid regex"), "{rendered}");

    // Correcting it applies.
    for _ in 0..8 {
        key(&mut app, &provider, KeyCode::Backspace);
    }
    type_text(&mut app, &provider, "/ok/");
    key(&mut app, &provider, KeyCode::Enter);
    assert_eq!(app.take_query_requests().len(), 1);
}

#[test]
fn an_engine_rejection_keeps_applied_rules_and_reports_the_rule_in_the_dialog() {
    let (provider, mut app) = demo();
    let accepted = ColorRule {
        predicate: "accepted".into(),
        color: RuleColor::Green,
        column: None,
        value: None,
    };
    let candidate = ColorRule {
        predicate: "missing.field: value".into(),
        color: RuleColor::Purple,
        column: None,
        value: None,
    };
    let state = app.views.active_mut().unwrap();
    state.color_rules = vec![accepted.clone()];
    state.color_rules_draft = vec![candidate.clone()];

    app.handle(Action::Open(Open::ColorRules), &provider);
    while app.layers.color_rules.control() != ColorRulesControl::Apply {
        key(&mut app, &provider, KeyCode::Tab);
    }
    key(&mut app, &provider, KeyCode::Enter);
    let request = app.take_query_requests().pop().expect("candidate repaint");
    assert_eq!(request.base_constraints.color_rules, vec![accepted.clone()]);
    assert_eq!(request.constraints.color_rules, vec![candidate.clone()]);
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: request.view_id,
        generation: request.generation,
        revision: request.revision,
        purpose: request.purpose,
        result: Err(lvu::QueryFailure {
            purpose: QueryPurpose::Advanced,
            message: "colour rule 1: field missing.field is unavailable".into(),
        }),
    }));

    let state = app.view_state().unwrap();
    assert_eq!(state.color_rules, vec![accepted]);
    assert_eq!(state.color_rules_draft, vec![candidate]);
    assert_eq!(
        state.color_rules_error.as_deref(),
        Some("colour rule 1: field missing.field is unavailable")
    );
    let rendered = screen(&draw(&provider, &mut app, 90, 24));
    assert!(rendered.contains("colour rule 1"), "{rendered}");
}

#[test]
fn the_list_is_bounded_and_an_abandoned_rule_removes_itself() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::ColorRules), &provider);
    draw(&provider, &mut app, 90, 24);
    let alt_a = Action::Raw(RawEvent::Key(KeyEvent::new(
        KeyCode::Char('a'),
        KeyModifiers::ALT,
    )));
    for index in 0..MAX_COLOR_RULES {
        app.handle(alt_a.clone(), &provider);
        type_text(&mut app, &provider, &format!("rule{index}"));
    }
    assert_eq!(
        app.view_state().unwrap().color_rules_draft.len(),
        MAX_COLOR_RULES
    );
    app.handle(alt_a.clone(), &provider);
    assert_eq!(
        app.view_state().unwrap().color_rules_draft.len(),
        MAX_COLOR_RULES,
        "the list is bounded"
    );
    assert!(
        app.view_state()
            .unwrap()
            .color_rules_error
            .as_deref()
            .is_some_and(|error| error.contains("at most"))
    );

    // A rule added and abandoned without a predicate leaves no empty rule
    // behind, because one would match nothing and paint nothing.
    key(&mut app, &provider, KeyCode::Backspace);
    let before = app.view_state().unwrap().color_rules_draft.len();
    key(&mut app, &provider, KeyCode::Esc);
    app.handle(Action::Open(Open::ColorRules), &provider);
    app.handle(alt_a, &provider);
    key(&mut app, &provider, KeyCode::Esc);
    assert_eq!(app.view_state().unwrap().color_rules_draft.len(), before);
}

#[test]
fn the_draft_survives_closing_and_seeds_from_the_accepted_rules() {
    let (provider, mut app) = demo();
    let view = app.active_view_id().unwrap().to_owned();
    app.handle(Action::Open(Open::ColorRules), &provider);
    draw(&provider, &mut app, 90, 24);
    app.handle(
        Action::Raw(RawEvent::Key(KeyEvent::new(
            KeyCode::Char('a'),
            KeyModifiers::ALT,
        ))),
        &provider,
    );
    type_text(&mut app, &provider, "unfinished");
    key(&mut app, &provider, KeyCode::Esc);
    app.handle(Action::Open(Open::ColorRules), &provider);
    assert_eq!(
        app.view_state().unwrap().color_rules_draft[0].predicate,
        "unfinished",
        "an unfinished edit resumes"
    );

    // A dialog opened against accepted rules with no draft seeds from them, so
    // Apply cannot read as "delete every rule".
    key(&mut app, &provider, KeyCode::Esc);
    if let Some(state) = app.views.active_mut() {
        state.color_rules_draft.clear();
        state.color_rules = vec![ColorRule {
            predicate: "accepted".into(),
            color: RuleColor::Green,
            column: None,
            value: None,
        }];
    }
    app.handle(Action::Open(Open::ColorRules), &provider);
    assert_eq!(
        app.view_state().unwrap().color_rules_draft,
        app.view_state().unwrap().color_rules
    );
    let _ = view;
}

#[test]
fn the_colour_is_chosen_by_key_and_by_clicking_its_swatch() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::ColorRules), &provider);
    draw(&provider, &mut app, 90, 24);
    app.handle(
        Action::Raw(RawEvent::Key(KeyEvent::new(
            KeyCode::Char('a'),
            KeyModifiers::ALT,
        ))),
        &provider,
    );
    type_text(&mut app, &provider, "warn");
    draw(&provider, &mut app, 90, 24);
    let first = app.view_state().unwrap().color_rules_draft[0].color;

    while app.layers.color_rules.control() != ColorRulesControl::Color {
        key(&mut app, &provider, KeyCode::Tab);
    }
    key(&mut app, &provider, KeyCode::Right);
    let next = app.view_state().unwrap().color_rules_draft[0].color;
    assert_ne!(next, first);
    key(&mut app, &provider, KeyCode::Left);
    assert_eq!(app.view_state().unwrap().color_rules_draft[0].color, first);

    // The swatch is drawn in the colour it names, and clicking it advances.
    draw(&provider, &mut app, 90, 24);
    let (rect, index) = app.layers.color_rules.row_rects()[0];
    assert_eq!(index, 0);
    assert_eq!(
        app.layers.color_rules.hit((rect.x, rect.y)),
        Some(ColorRulesHit::Row(0))
    );
    let swatch = (rect.x + 4, rect.y);
    assert_eq!(
        app.layers.color_rules.hit(swatch),
        Some(ColorRulesHit::Swatch(0))
    );
    click(&mut app, &provider, swatch);
    assert_ne!(app.view_state().unwrap().color_rules_draft[0].color, first);
}

#[test]
fn a_matched_rule_paints_the_row_over_the_value_colour() {
    let theme = Theme::TERMINAL;
    let (_, sources, views) = FixtureProvider::demo();
    let mut app = App::new(sources, views, true);
    let provider = Painted(vec![
        painted_row(1, "first row", Some(2)),
        painted_row(2, "second row", None),
    ]);
    if let Some(state) = app.views.active_mut() {
        state.color_rules = vec![
            ColorRule {
                predicate: "never".into(),
                color: RuleColor::Blue,
                column: None,
                value: None,
            },
            ColorRule {
                predicate: "row".into(),
                color: RuleColor::Magenta,
                column: None,
                value: None,
            },
        ];
        // A colour field would normally decide the row's colour; an explicit
        // rule outranks it.
        state.color_field = Some("raw".into());
    }
    app.sync_provider(&provider, 10);
    let buffer = draw(&provider, &mut app, 80, 12);
    let rendered = screen(&buffer);
    let row = rendered
        .lines()
        .position(|line| line.contains("first row"))
        .expect("the painted row is drawn");
    let cell = (0..buffer.area.width)
        .find(|x| buffer[(*x, row as u16)].symbol() == "f")
        .expect("the event text");
    assert_eq!(
        buffer[(cell, row as u16)].fg,
        theme.rule_color(RuleColor::Magenta),
        "the second rule painted it, because it is the one that matched"
    );

    let plain = rendered
        .lines()
        .position(|line| line.contains("second row"))
        .expect("the unpainted row is drawn");
    let plain_cell = (0..buffer.area.width)
        .find(|x| buffer[(*x, plain as u16)].symbol() == "s")
        .expect("the event text");
    assert_ne!(
        buffer[(plain_cell, plain as u16)].fg,
        theme.rule_color(RuleColor::Magenta),
        "a row the engine did not report is not painted"
    );
}

#[test]
fn a_rule_index_the_terminal_no_longer_has_paints_nothing() {
    // The engine reported rule 4 and the list has two: rather than painting the
    // wrong colour, the row falls back to its other colours.
    let (_, sources, views) = FixtureProvider::demo();
    let mut app = App::new(sources, views, true);
    let provider = Painted(vec![painted_row(1, "stale", Some(4))]);
    if let Some(state) = app.views.active_mut() {
        state.color_rules = vec![ColorRule {
            predicate: "stale".into(),
            color: RuleColor::Magenta,
            column: None,
            value: None,
        }];
    }
    app.sync_provider(&provider, 10);
    let buffer = draw(&provider, &mut app, 80, 12);
    let rendered = screen(&buffer);
    let row = rendered
        .lines()
        .position(|line| line.contains("stale"))
        .expect("drawn");
    let cell = (0..buffer.area.width)
        .find(|x| buffer[(*x, row as u16)].symbol() == "s")
        .expect("text");
    assert_ne!(
        buffer[(cell, row as u16)].fg,
        Theme::TERMINAL.rule_color(RuleColor::Magenta)
    );
}

#[test]
fn a_matched_span_is_underlined_inside_the_line_it_was_found_in() {
    let (_, sources, views) = FixtureProvider::demo();
    let mut app = App::new(sources, views, true);
    // Invalid UTF-8 renders as U+FFFD; the highlight must still land on the
    // needle, not three columns to its left.
    let lossy = String::from_utf8_lossy(b"head \xff\xfe tail needle end").into_owned();
    let provider = Painted(vec![painted_row(1, &lossy, None)]);
    if let Some(state) = app.views.active_mut() {
        state.search.applied = "needle".into();
    }
    app.sync_provider(&provider, 10);
    let buffer = draw(&provider, &mut app, 90, 12);
    let rendered = screen(&buffer);
    let row = rendered
        .lines()
        .position(|line| line.contains("needle"))
        .expect("the row is drawn") as u16;
    let start = column_of(&buffer, row, "needle");
    for offset in 0..6 {
        assert!(
            buffer[(start + offset, row)]
                .modifier
                .contains(Modifier::UNDERLINED),
            "column {offset} of the match is not emphasised:\n{rendered}"
        );
    }
    assert!(
        !buffer[(start - 1, row)]
            .modifier
            .contains(Modifier::UNDERLINED),
        "the emphasis leaked past the match:\n{rendered}"
    );
}

#[test]
fn a_rule_pattern_is_highlighted_and_a_column_predicate_is_not() {
    let (_, sources, views) = FixtureProvider::demo();
    let mut app = App::new(sources, views, true);
    let provider = Painted(vec![painted_row(1, "status 503 ready", None)]);
    if let Some(state) = app.views.active_mut() {
        state.color_rules = vec![
            ColorRule {
                predicate: r"/\d+/".into(),
                color: RuleColor::Red,
                column: None,
                value: None,
            },
            // Names a column, so there is no run of characters to underline.
            ColorRule {
                predicate: "level: INFO".into(),
                color: RuleColor::Blue,
                column: None,
                value: None,
            },
        ];
    }
    app.sync_provider(&provider, 10);
    let buffer = draw(&provider, &mut app, 90, 12);
    let rendered = screen(&buffer);
    let row = rendered
        .lines()
        .position(|line| line.contains("status 503"))
        .expect("drawn") as u16;
    let digits = column_of(&buffer, row, "503");
    for offset in 0..3 {
        assert!(
            buffer[(digits + offset, row)]
                .modifier
                .contains(Modifier::UNDERLINED),
            "{rendered}"
        );
    }
    let ready = column_of(&buffer, row, "ready");
    assert!(
        !buffer[(ready, row)].modifier.contains(Modifier::UNDERLINED),
        "a column predicate underlined arbitrary text:\n{rendered}"
    );
}

#[test]
fn the_layer_declines_to_open_without_a_view_and_dismisses_to_the_base_focus() {
    let (provider, sources, _) = FixtureProvider::demo();
    let mut app = App::new(sources, Vec::new(), true);
    app.handle(Action::Open(Open::ColorRules), &provider);
    // An empty workspace opens on Add source; what matters is that the colour
    // layer declined, not what else is on the stack.
    assert!(
        !app.layers.color_rules.is_open(),
        "nothing to paint, nothing opens"
    );
    assert!(!app.layers.stack.contains(&LayerId::ColorRules));

    let (provider, mut app) = demo();
    app.focus = Focus::Selector;
    app.handle(Action::Open(Open::ColorRules), &provider);
    let surface = {
        draw(&provider, &mut app, 80, 24);
        app.layers.color_rules.surface()
    };
    assert_eq!(app.hit_regions.selection_modal, Some(surface.interior));
    key(&mut app, &provider, KeyCode::Esc);
    assert!(app.layers.stack.is_empty());
    assert_eq!(app.focus, Focus::Selector);
}

/// Rows proving evaluated enrichment outputs through structural markers, as
/// the view worker serves them: `derived.` declares the output,
/// `derived_ready.` proves the cell evaluated without failure. The output
/// inventory rides the provider API rather than paging, so tests can serve
/// zero rows while the accepted chain still declares outputs.
struct Classified {
    rows: Vec<DisplayRow>,
    outputs: Vec<String>,
}

fn classified_row(sequence: u64, level: &str, severity: Option<&str>) -> DisplayRow {
    // Both outputs read as evaluated, as if the accepted chain derived them:
    // `level` lowercases the raw field, `severity` maps it to a token.
    let mut details = vec![
        ("derived.level".to_owned(), level.to_owned()),
        ("derived_ready.level".to_owned(), level.to_owned()),
    ];
    let mut fields = vec![("level".to_owned(), level.to_owned())];
    if let Some(value) = severity {
        details.push(("derived.severity".to_owned(), value.to_owned()));
        details.push(("derived_ready.severity".to_owned(), value.to_owned()));
        fields.push(("severity".to_owned(), value.to_owned()));
    }
    DisplayRow {
        id: RowId::new("api", sequence),
        timestamp: "12:00:00".into(),
        captured_at_unix_nanos: Some(1),
        level: String::new(),
        text: format!("level={level} request {sequence:02}"),
        details,
        fields,
    }
}

impl RowProvider for Classified {
    fn page(&self, _: &str, request: ViewportRequest) -> RowPage {
        RowPage {
            total: self.rows.len(),
            rows: self
                .rows
                .iter()
                .skip(request.start)
                .take(request.len.max(1))
                .cloned()
                .collect(),
        }
    }
    fn row_by_id(&self, _: &str, id: &RowId) -> Option<DisplayRow> {
        self.rows.iter().find(|row| &row.id == id).cloned()
    }
    fn index_of_id(&self, _: &str, id: &RowId) -> Option<usize> {
        self.rows.iter().position(|row| &row.id == id)
    }
    fn revision(&self, _: &str) -> u64 {
        1
    }
    fn enrichment_outputs(&self, _: &str) -> Vec<String> {
        self.outputs.clone()
    }
}

fn classified_demo() -> (Classified, App) {
    let (_, sources, views) = FixtureProvider::demo();
    let provider = Classified {
        rows: vec![
            classified_row(1, "warn", Some("WARN")),
            classified_row(2, "info", Some("INFO")),
        ],
        outputs: vec!["level".to_owned(), "severity".to_owned()],
    };
    let mut app = App::new(sources, views, true);
    app.sync_provider(&provider, 10);
    (provider, app)
}

#[test]
fn add_classifies_an_enrichment_column_instead_of_a_raw_pattern() {
    let (provider, mut app) = classified_demo();
    app.handle(Action::Open(Open::ColorRules), &provider);
    draw(&provider, &mut app, 90, 24);
    // Normal entry starts life as a column rule over a proven output — never
    // as an independent raw pattern classifier. The seed is the first
    // classifiable output in order.
    key(&mut app, &provider, KeyCode::Enter);
    {
        let draft = &app.view_state().unwrap().color_rules_draft;
        assert_eq!(draft.len(), 1);
        assert!(
            draft[0].is_column(),
            "Add with outputs present classifies: {:?}",
            draft[0]
        );
        assert_eq!(draft[0].column.as_deref(), Some("level"));
    }
    // An empty-string value is valid: it matches only literal empty-string
    // ready cells, distinctly from a missing value (which execution
    // rejects). Authoring text settles the new rule, so clearing it back to
    // empty reads as an empty edit — not an abandoned addition, which
    // removes itself instead — and applies as an explicit empty match.
    type_text(&mut app, &provider, "w");
    key(&mut app, &provider, KeyCode::Backspace);
    assert!(
        app.view_state().unwrap().color_rules_draft[0]
            .value
            .as_deref()
            .unwrap_or_default()
            .is_empty()
    );
    key(&mut app, &provider, KeyCode::Enter);
    let request = app.take_query_requests().pop().expect("one repaint query");
    assert_eq!(
        request.constraints.color_rules,
        vec![ColorRule::column_rule(
            "level".into(),
            String::new(),
            RuleColor::Red,
        )],
        "an empty value travels as an explicit empty match"
    );
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: request.view_id,
        generation: request.generation,
        revision: request.revision,
        purpose: request.purpose,
        result: Ok(()),
    }));
    // The value field takes the exact key; re-applying carries a column rule.
    type_text(&mut app, &provider, "warn");
    key(&mut app, &provider, KeyCode::Enter);
    let request = app.take_query_requests().pop().expect("one repaint query");
    assert_eq!(
        request.constraints.color_rules,
        vec![ColorRule::column_rule(
            "level".into(),
            "warn".into(),
            RuleColor::Red,
        )],
        "a column rule travels to the engine as a constraint"
    );
}

#[test]
fn the_column_chooser_repoints_without_rewriting_the_value() {
    let (provider, mut app) = classified_demo();
    app.handle(Action::Open(Open::ColorRules), &provider);
    draw(&provider, &mut app, 90, 24);
    key(&mut app, &provider, KeyCode::Enter);
    type_text(&mut app, &provider, "warn");
    // Tab reaches the Column chooser; Right repoints to the next output.
    while app.layers.color_rules.control() != ColorRulesControl::Column {
        key(&mut app, &provider, KeyCode::Tab);
    }
    let rendered = screen(&draw(&provider, &mut app, 90, 24));
    assert!(rendered.contains("Column"), "{rendered}");
    key(&mut app, &provider, KeyCode::Right);
    {
        let rule = &app.view_state().unwrap().color_rules_draft[0];
        assert_eq!(rule.column.as_deref(), Some("severity"));
        assert_eq!(
            rule.value.as_deref(),
            Some("warn"),
            "repointing keeps the value"
        );
    }
    key(&mut app, &provider, KeyCode::Left);
    assert_eq!(
        app.view_state().unwrap().color_rules_draft[0]
            .column
            .as_deref(),
        Some("level")
    );
}

#[test]
fn pending_or_empty_pages_still_classify_accepted_outputs() {
    // The inventory is the accepted membership's, not the served page: with
    // zero rows served but outputs declared, Add must still create the same
    // column rule — never silently fall back to a raw predicate.
    for name in ["pending page", "settled zero-row filter"] {
        let (_, sources, views) = FixtureProvider::demo();
        let provider = Classified {
            rows: Vec::new(),
            outputs: vec!["severity".to_owned()],
        };
        let mut app = App::new(sources, views, true);
        app.sync_provider(&provider, 10);
        app.handle(Action::Open(Open::ColorRules), &provider);
        draw(&provider, &mut app, 90, 24);
        key(&mut app, &provider, KeyCode::Enter);
        let draft = &app.view_state().unwrap().color_rules_draft;
        assert_eq!(draft.len(), 1, "{name}");
        assert!(
            draft[0].is_column(),
            "{name} must classify, not match raw: {:?}",
            draft[0]
        );
        assert_eq!(draft[0].column.as_deref(), Some("severity"), "{name}");
    }
}

#[test]
fn raw_text_stays_an_explicit_exception_without_outputs() {
    // Fixture rows carry no derived markers: there is nothing to classify,
    // so Add starts the explicit raw-text exception instead of a column rule.
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::ColorRules), &provider);
    draw(&provider, &mut app, 90, 24);
    key(&mut app, &provider, KeyCode::Enter);
    {
        let draft = &app.view_state().unwrap().color_rules_draft;
        assert_eq!(draft.len(), 1);
        assert!(
            !draft[0].is_column(),
            "no outputs means the raw-text exception: {:?}",
            draft[0]
        );
    }
    type_text(&mut app, &provider, "timeout");
    assert_eq!(
        app.view_state().unwrap().color_rules_draft[0].predicate,
        "timeout"
    );
}

fn tab_to_add(app: &mut App, provider: &FixtureProvider) {
    for _ in 0..16 {
        if app.layers.color_rules.control() == ColorRulesControl::Add {
            return;
        }
        key(app, provider, KeyCode::Tab);
    }
    panic!("Tab never reached Add");
}

/// Every planned action/More rect must be full-size (at least its required
/// button width), inside the band, and pairwise disjoint: Ratatui squeezes
/// over-wide fixed Length constraints instead of refusing, so anything less
/// is a clipped label or a dead hitbox masquerading as geometry.
fn assert_action_rects_valid(
    band: ratatui::layout::Rect,
    buttons: &[(usize, ratatui::layout::Rect)],
    more: Option<ratatui::layout::Rect>,
    labels: &[&str],
    tag: &str,
) {
    use lvu::dialog_controls::{MORE_LABEL, button_width};
    let mut seen: Vec<ratatui::layout::Rect> = Vec::new();
    for (index, rect) in buttons {
        let required = button_width(labels[*index]);
        assert!(
            rect.width >= required,
            "{tag}: button {} paints {} wide, needs {required}",
            labels[*index],
            rect.width,
        );
        assert!(
            rect.x >= band.x
                && rect.right() <= band.right()
                && rect.y >= band.y
                && rect.bottom() <= band.bottom(),
            "{tag}: button {} at {rect:?} escapes band {band:?}",
            labels[*index],
        );
        assert!(
            seen.iter().all(|prior: &ratatui::layout::Rect| {
                prior.x >= rect.right()
                    || rect.x >= prior.right()
                    || prior.y >= rect.bottom()
                    || rect.y >= prior.bottom()
            }),
            "{tag}: button {} at {rect:?} overlaps {seen:?}",
            labels[*index],
        );
        seen.push(*rect);
    }
    if let Some(rect) = more {
        let required = button_width(MORE_LABEL);
        assert!(
            rect.width >= required,
            "{tag}: More paints {} wide, needs {required}",
            rect.width
        );
        assert!(
            rect.x >= band.x
                && rect.right() <= band.right()
                && rect.y >= band.y
                && rect.bottom() <= band.bottom(),
            "{tag}: More at {rect:?} escapes band {band:?}",
        );
        assert!(
            seen.iter().all(|prior: &ratatui::layout::Rect| {
                prior.x >= rect.right()
                    || rect.x >= prior.right()
                    || prior.y >= rect.bottom()
                    || rect.y >= prior.bottom()
            }),
            "{tag}: More at {rect:?} overlaps {seen:?}",
        );
    }
}

#[test]
fn overflow_menu_activates_every_hidden_verb_at_the_floor() {
    // At 20x6 the two-row band holds a full Apply plus a full More; Add and
    // Remove hide behind the menu. The kept band height equals the requested
    // budget: degradation sheds nothing.
    let (provider, mut app) = demo();
    draw(&provider, &mut app, 20, 6);
    app.handle(Action::Open(Open::ColorRules), &provider);
    tab_to_add(&mut app, &provider);
    key(&mut app, &provider, KeyCode::Enter);
    type_text(&mut app, &provider, "rx");
    draw(&provider, &mut app, 20, 6);
    assert_eq!(app.view_state().unwrap().color_rules_draft.len(), 1);
    assert_eq!(app.layers.color_rules.action_band().height, 2);
    let more = app
        .layers
        .color_rules
        .more_button()
        .expect("More paints at 20x6");
    // Only Apply paints directly; Add/Remove are hidden but live in overflow.
    // Every planned rect is full-size, in-band and disjoint: no squeezed
    // Lengths, no clipped labels.
    assert_action_rects_valid(
        app.layers.color_rules.action_band(),
        &[(
            2,
            app.layers
                .color_rules
                .control_rects()
                .iter()
                .find_map(|(rect, control)| (*control == ColorRulesControl::Apply).then_some(*rect))
                .expect("Apply paints"),
        )],
        Some(more),
        &["&Add", "&Remove", "A&pply"],
        "20x6 floor",
    );
    let floor_screen = screen(&draw(&provider, &mut app, 20, 6));
    assert!(floor_screen.contains("[ Apply ]"), "{floor_screen}");
    assert!(floor_screen.contains("[ More ▾ ]"), "{floor_screen}");
    assert!(
        app.layers
            .color_rules
            .control_rects()
            .iter()
            .any(|(_, painted)| *painted == ColorRulesControl::Apply),
        "Apply paints directly at 20x6"
    );
    assert!(
        !app.layers
            .color_rules
            .control_rects()
            .iter()
            .any(|(_, control)| {
                matches!(control, ColorRulesControl::Add | ColorRulesControl::Remove)
            })
    );

    // Hidden-focus Enter opens the menu on that verb instead of running it
    // blind: the draft is untouched by the first Enter (menu opened on Add),
    // and only the second Enter runs Add through `press_action`.
    key(&mut app, &provider, KeyCode::Tab);
    key(&mut app, &provider, KeyCode::Tab);
    assert_eq!(app.layers.color_rules.control(), ColorRulesControl::Add);
    draw(&provider, &mut app, 20, 6);
    key(&mut app, &provider, KeyCode::Enter);
    draw(&provider, &mut app, 20, 6);
    let rows = app.layers.color_rules.more_rows().to_vec();
    assert!(!rows.is_empty(), "the menu paints");
    assert_eq!(rows[0].1, 0, "menu opens on the focused verb");
    for (rect, _) in &rows {
        assert!(rect.right() <= 20 && rect.bottom() <= 6);
    }
    // One-row gap against the More button that anchored the menu.
    let menu_top = rows[0].0.y.saturating_sub(1);
    let menu_bottom = rows.last().map(|(rect, _)| rect.y.saturating_add(2));
    assert!(
        menu_top == more.bottom().saturating_add(1)
            || menu_bottom.is_some_and(|bottom| bottom.saturating_add(1) == more.y),
        "menu keeps its one-row gap to More {more:?}"
    );
    assert_eq!(app.view_state().unwrap().color_rules_draft.len(), 1);
    key(&mut app, &provider, KeyCode::Enter);
    assert_eq!(app.view_state().unwrap().color_rules_draft.len(), 2);
    draw(&provider, &mut app, 20, 6);
    assert!(app.layers.color_rules.more_rows().is_empty());

    // Mouse: the More button opens; a row click activates immediately. Down
    // first scrolls the one-row menu to Remove, which then removes the
    // selected rule exactly like its button.
    let more = app.layers.color_rules.more_button().expect("More paints");
    click(&mut app, &provider, (more.x, more.y));
    draw(&provider, &mut app, 20, 6);
    assert!(!app.layers.color_rules.more_rows().is_empty());
    key(&mut app, &provider, KeyCode::Down);
    draw(&provider, &mut app, 20, 6);
    let rows = app.layers.color_rules.more_rows().to_vec();
    let remove = rows
        .iter()
        .find_map(|(rect, index)| (*index == 1).then_some(*rect))
        .expect("Remove painted after scroll");
    click(&mut app, &provider, (remove.x, remove.y));
    assert_eq!(app.view_state().unwrap().color_rules_draft.len(), 1);

    // Escape closes the menu and keeps the dialog with its draft. Redraw
    // first: hit-testing always reads the last render, as in the live loop.
    draw(&provider, &mut app, 20, 6);
    let more = app.layers.color_rules.more_button().expect("More paints");
    click(&mut app, &provider, (more.x, more.y));
    draw(&provider, &mut app, 20, 6);
    assert!(!app.layers.color_rules.more_rows().is_empty());
    key(&mut app, &provider, KeyCode::Esc);
    draw(&provider, &mut app, 20, 6);
    assert!(app.layers.color_rules.more_rows().is_empty());
    assert!(app.layers.color_rules.is_open());
    assert_eq!(app.view_state().unwrap().color_rules_draft.len(), 1);

    // Roomy sizes fit every verb with a one-row band: no overflow invented.
    for (width, height) in [(80u16, 24u16), (54, 16)] {
        let (provider, mut app) = demo();
        draw(&provider, &mut app, width, height);
        app.handle(Action::Open(Open::ColorRules), &provider);
        tab_to_add(&mut app, &provider);
        key(&mut app, &provider, KeyCode::Enter);
        type_text(&mut app, &provider, "rx");
        draw(&provider, &mut app, width, height);
        assert!(
            app.layers.color_rules.more_button().is_none(),
            "{width}x{height}: no overflow invented"
        );
        assert_eq!(
            app.layers.color_rules.action_band().height,
            1,
            "{width}x{height}: one action row, no dead row"
        );
        let mut painted = Vec::new();
        for control in [
            ColorRulesControl::Add,
            ColorRulesControl::Remove,
            ColorRulesControl::Apply,
        ] {
            let rect = app
                .layers
                .color_rules
                .control_rects()
                .iter()
                .find_map(|(rect, painted)| (*painted == control).then_some(*rect))
                .unwrap_or_else(|| panic!("{width}x{height}: {control:?} paints directly"));
            painted.push((
                match control {
                    ColorRulesControl::Add => 0,
                    ColorRulesControl::Remove => 1,
                    _ => 2,
                },
                rect,
            ));
        }
        assert_action_rects_valid(
            app.layers.color_rules.action_band(),
            &painted,
            None,
            &["&Add", "&Remove", "A&pply"],
            &format!("{width}x{height} roomy"),
        );
        let rendered = screen(&draw(&provider, &mut app, width, height));
        assert!(rendered.contains("[ Add ]"), "{rendered}");
        assert!(rendered.contains("[ Remove ]"), "{rendered}");
        assert!(rendered.contains("[ Apply ]"), "{rendered}");
    }
}

#[test]
fn inspector_frame_is_stable_across_empty_dirty_error_and_applied_states() {
    let (provider, mut app) = demo();
    draw(&provider, &mut app, 100, 28);
    app.handle(Action::Open(Open::ColorRules), &provider);
    draw(&provider, &mut app, 100, 28);
    let anchor = app.shell.context_anchor.expect("anchor frozen at open");
    let empty = app.layers.color_rules.surface();
    assert!(screen(&draw(&provider, &mut app, 100, 28)).contains("no rules"));

    // Dirty: a typed-but-unapplied draft. The message row changes state, the
    // frame must not.
    tab_to_add(&mut app, &provider);
    key(&mut app, &provider, KeyCode::Enter);
    assert_eq!(
        app.layers.color_rules.control(),
        ColorRulesControl::Predicate
    );
    type_text(&mut app, &provider, "timeout");
    draw(&provider, &mut app, 100, 28);
    assert!(
        screen(&draw(&provider, &mut app, 100, 28)).contains("edited · Apply"),
        "dirty state shows"
    );
    let dirty = app.layers.color_rules.surface();

    // Error: an invalid regex is refused where it was typed; the draft and
    // the frame both stay put.
    for _ in 0.."timeout".len() {
        key(&mut app, &provider, KeyCode::Backspace);
    }
    type_text(&mut app, &provider, "/[/");
    key(&mut app, &provider, KeyCode::Enter);
    let errored = screen(&draw(&provider, &mut app, 100, 28));
    assert!(errored.contains("invalid regex"), "{errored}");
    let error = app.layers.color_rules.surface();
    assert_eq!(
        app.view_state().unwrap().color_rules_draft[0].predicate,
        "/[/",
        "a refused draft is kept for correction"
    );

    // Applied: fix the predicate, apply, and land the engine's answer. The
    // accepted list repaints in the same frame the empty list used.
    for _ in 0.."/[/".len() {
        key(&mut app, &provider, KeyCode::Backspace);
    }
    type_text(&mut app, &provider, "timeout");
    key(&mut app, &provider, KeyCode::Enter);
    let request = app.take_query_requests().pop().expect("one repaint query");
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: request.view_id,
        generation: request.generation,
        revision: request.revision,
        purpose: request.purpose,
        result: Ok(()),
    }));
    let applied = screen(&draw(&provider, &mut app, 100, 28));
    assert!(applied.contains("1 rule painting this view"), "{applied}");
    let settled = app.layers.color_rules.surface();

    for (name, surface) in [("dirty", dirty), ("error", error), ("applied", settled)] {
        assert_eq!(surface.popup, empty.popup, "{name} state moved the frame");
        assert_eq!(
            surface.interior, empty.interior,
            "{name} state moved the interior"
        );
    }
    assert_eq!(
        app.shell.context_anchor,
        Some(anchor),
        "the anchor is retained across frames, never recaptured"
    );
}

#[test]
fn inspector_avoids_the_frozen_selected_row_with_a_one_row_gap() {
    for (width, height) in [(240u16, 80u16), (140, 40), (80, 24), (54, 16)] {
        let tag = format!("{width}x{height}");
        let (provider, mut app) = demo();
        draw(&provider, &mut app, width, height);
        app.handle(Action::Open(Open::ColorRules), &provider);
        let anchor = app.shell.context_anchor.expect("anchor frozen at open");
        assert!(
            !anchor.row.is_empty(),
            "{tag}: the opening selection names a row"
        );
        draw(&provider, &mut app, width, height);
        let frame = app.layers.color_rules.surface().popup;
        assert!(
            frame.right() <= width && frame.bottom() <= height,
            "{tag}: frame {frame:?} escapes the area"
        );
        assert!(
            frame.bottom() <= anchor.row.y || frame.y >= anchor.row.bottom(),
            "{tag}: frame {frame:?} covers referent row {:?}",
            anchor.row
        );
        let below = frame.y == anchor.row.bottom().saturating_add(1);
        let above = frame.bottom().saturating_add(1) == anchor.row.y;
        assert!(
            below || above,
            "{tag}: frame {frame:?} keeps no one-row gap to row {:?}",
            anchor.row
        );
        assert_eq!(
            app.shell.context_anchor,
            Some(anchor),
            "{tag}: live frames must not chase the selection"
        );
    }

    // The 20x6 safety floor owns the whole frame; avoidance is impossible and
    // the dialog takes it explicitly rather than drawing a broken inspector.
    let (provider, mut app) = demo();
    draw(&provider, &mut app, 20, 6);
    app.handle(Action::Open(Open::ColorRules), &provider);
    draw(&provider, &mut app, 20, 6);
    assert_eq!(
        app.layers.color_rules.surface().popup,
        ratatui::layout::Rect::new(0, 0, 20, 6)
    );
    assert!(screen(&draw(&provider, &mut app, 20, 6)).contains("Colour rules"));

    // Below the floor the existing tiny fallback owns the screen instead.
    let tiny = screen(&draw(&provider, &mut app, 19, 5));
    assert!(tiny.contains("terminal too small"), "{tiny}");
    assert!(!tiny.contains("Colour rules"), "{tiny}");
}

#[test]
fn deep_selection_editor_and_mouse_stay_on_painted_geometry() {
    let (provider, mut app) = demo();
    draw(&provider, &mut app, 100, 28);
    app.handle(Action::Open(Open::ColorRules), &provider);
    for i in 0..10 {
        tab_to_add(&mut app, &provider);
        key(&mut app, &provider, KeyCode::Enter);
        type_text(&mut app, &provider, &format!("rule{i}"));
        key(&mut app, &provider, KeyCode::Tab);
        key(&mut app, &provider, KeyCode::Tab);
    }
    assert_eq!(app.view_state().unwrap().color_rules_draft.len(), 10);
    assert_eq!(app.layers.color_rules.selected(), 9);

    // The freshly added last rule is revealed, not left below the fold, with
    // its editor value on screen.
    draw(&provider, &mut app, 54, 16);
    let rows: Vec<(u16, usize)> = app
        .layers
        .color_rules
        .row_rects()
        .iter()
        .map(|(rect, index)| (rect.y, *index))
        .collect();
    assert!(
        rows.iter().any(|(_, index)| *index == 9),
        "selected rule 9 is painted: {rows:?}"
    );
    assert!(
        screen(&draw(&provider, &mut app, 54, 16)).contains("rule9"),
        "the selected rule's value is visible"
    );

    // Wrapping around reveals the other end through the same shared viewport.
    key(&mut app, &provider, KeyCode::Down);
    assert_eq!(app.layers.color_rules.selected(), 0);
    draw(&provider, &mut app, 54, 16);
    assert!(
        app.layers
            .color_rules
            .row_rects()
            .iter()
            .any(|(_, i)| *i == 0),
        "wrapped selection repaints at the top"
    );

    // Every painted row hit-tests back to its own index, and clicking one
    // selects it exactly like the arrow keys do.
    for (rect, index) in app.layers.color_rules.row_rects().to_vec() {
        assert_eq!(
            app.layers.color_rules.hit((rect.x, rect.y)),
            Some(ColorRulesHit::Row(index)),
            "row {rect:?} does not hit-test to rule {index}"
        );
    }
    let (middle, middle_index) =
        app.layers.color_rules.row_rects()[app.layers.color_rules.row_rects().len() / 2];
    click(&mut app, &provider, (middle.x, middle.y));
    assert_eq!(app.layers.color_rules.selected(), middle_index);
    assert_eq!(app.layers.color_rules.control(), ColorRulesControl::List);

    // The editor row for the clicked rule is one Tab away with a live caret,
    // however deep the list scrolled to show it.
    key(&mut app, &provider, KeyCode::Tab);
    assert_eq!(
        app.layers.color_rules.control(),
        ColorRulesControl::Predicate
    );
    draw(&provider, &mut app, 54, 16);
    assert!(
        app.layers.color_rules.surface().caret.is_some(),
        "the predicate caret paints once the editor is revealed"
    );

    // At the 20x6 floor the two-row band holds a full Apply plus a full More
    // while the message row drops to one: the dialog stays usable, Add and
    // Remove live in the menu, and the kept band height still equals the
    // requested budget.
    draw(&provider, &mut app, 20, 6);
    assert!(app.layers.color_rules.is_open());
    assert!(screen(&draw(&provider, &mut app, 20, 6)).contains("Colour rules"));
    assert_eq!(app.layers.color_rules.action_band().height, 2);
    assert!(app.layers.color_rules.more_button().is_some());
    assert_eq!(
        app.view_state().unwrap().color_rules_draft.len(),
        10,
        "the draft survives the floor"
    );
    // Every control is Tab-reachable; hidden Add/Remove show the More ring
    // instead of landing invisibly, and the predicate still shows its caret
    // once the shared viewport reveals its editor row.
    let mut visited = vec![app.layers.color_rules.control()];
    let mut saw_caret = app.layers.color_rules.surface().caret.is_some();
    for _ in 0..6 {
        key(&mut app, &provider, KeyCode::Tab);
        let buffer = draw(&provider, &mut app, 20, 6);
        visited.push(app.layers.color_rules.control());
        saw_caret = saw_caret || app.layers.color_rules.surface().caret.is_some();
        if matches!(
            app.layers.color_rules.control(),
            ColorRulesControl::Add | ColorRulesControl::Remove
        ) && !app
            .layers
            .color_rules
            .control_rects()
            .iter()
            .any(|(_, painted)| *painted == app.layers.color_rules.control())
        {
            let more = app.layers.color_rules.more_button().expect("More paints");
            let cell = &buffer[(more.x, more.y)];
            assert!(
                cell.modifier.contains(ratatui::style::Modifier::BOLD),
                "More carries the focus ring for hidden {:?}",
                app.layers.color_rules.control(),
            );
        }
    }
    for control in [
        ColorRulesControl::List,
        ColorRulesControl::Predicate,
        ColorRulesControl::Color,
        ColorRulesControl::Add,
        ColorRulesControl::Remove,
        ColorRulesControl::Apply,
    ] {
        assert!(
            visited.contains(&control),
            "unreachable at 20x6: {control:?}"
        );
    }
    assert!(saw_caret, "no predicate caret at 20x6");

    // Hidden-focus Enter opens the menu on that verb; a second Enter runs it.
    let mut tabs = 0;
    while app.layers.color_rules.control() != ColorRulesControl::Add && tabs < 8 {
        key(&mut app, &provider, KeyCode::Tab);
        tabs += 1;
    }
    assert_eq!(app.layers.color_rules.control(), ColorRulesControl::Add);
    draw(&provider, &mut app, 20, 6);
    key(&mut app, &provider, KeyCode::Enter);
    draw(&provider, &mut app, 20, 6);
    assert!(!app.layers.color_rules.more_rows().is_empty());
    assert_eq!(app.view_state().unwrap().color_rules_draft.len(), 10);
    key(&mut app, &provider, KeyCode::Enter);
    assert_eq!(app.view_state().unwrap().color_rules_draft.len(), 11);

    // Regrowing restores the full dialog from the kept draft.
    draw(&provider, &mut app, 54, 16);
    assert!(
        !app.layers.color_rules.row_rects().is_empty(),
        "paint returns on regrow"
    );
}

#[test]
fn wide_and_combining_text_keep_exact_bytes_and_a_display_width_caret() {
    let (provider, mut app) = demo();
    draw(&provider, &mut app, 80, 24);
    app.handle(Action::Open(Open::ColorRules), &provider);
    tab_to_add(&mut app, &provider);
    key(&mut app, &provider, KeyCode::Enter);
    assert_eq!(
        app.layers.color_rules.control(),
        ColorRulesControl::Predicate
    );

    // A wide CJK character takes two cells, a combining mark takes none: the
    // caret must advance by display width, never by char count, and the draft
    // must keep the exact bytes.
    let mut columns = Vec::new();
    for character in ['a', '東', 'e', '́', 'x'] {
        key(&mut app, &provider, KeyCode::Char(character));
        draw(&provider, &mut app, 80, 24);
        let caret = app
            .layers
            .color_rules
            .surface()
            .caret
            .expect("caret paints");
        columns.push(caret.0);
    }
    assert_eq!(
        app.view_state().unwrap().color_rules_draft[0].predicate,
        "a東éx",
        "combining bytes are preserved, not normalized"
    );
    let deltas: Vec<u16> = columns
        .windows(2)
        .map(|pair| pair[1].saturating_sub(pair[0]))
        .collect();
    assert_eq!(
        deltas,
        vec![2, 1, 0, 1],
        "caret advances by display width: {columns:?}"
    );
}
