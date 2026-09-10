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
