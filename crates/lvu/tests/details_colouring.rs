//! The docked Details pane (`d`) and the log pane show the same records, so
//! they must colour them the same way.
//!
//! Details was written before the truecolor identity palette and the JSON
//! lexer landed and never caught up: it painted every value in one flat
//! `description` role on the dialog surface. These tests hold the two panes
//! together on the record's own colours — the severity or hashed row colour,
//! the JSON token colours inside the raw line, and the identity colour a
//! column's name carries — at both colour depths.

use lvu::{
    Action, App, ColorRule, DisplayRow, RowId, RowPage, RowProvider, RuleColor, ViewportRequest,
    app::{SourceItem, ViewItem},
    theme::{ColorDepth, Theme},
    ui,
};
use ratatui::{
    Terminal,
    backend::TestBackend,
    buffer::Buffer,
    style::{Color, Modifier},
};

/// The acceptance size for this pane.
const SIZE: (u16, u16) = (80, 24);

/// Two records whose raw text is valid JSON — so the log's lexer has something
/// to colour — short enough to fit the event column at 80 columns unclipped.
struct Records {
    rows: Vec<DisplayRow>,
}

const QUIET: &str = r#"{"svc":"ship","n":1,"ok":true}"#;
const LOUD: &str = r#"{"svc":"idx","n":3,"bad":null}"#;

impl Records {
    fn new() -> Self {
        let make = |sequence: u64, level: &str, text: &str, service: &str| DisplayRow {
            id: RowId::new("api", sequence),
            timestamp: format!("12:00:{sequence:02}"),
            captured_at_unix_nanos: Some(sequence as i64),
            level: level.into(),
            text: text.into(),
            details: vec![("origin".into(), "fixture".into())],
            fields: vec![
                ("svc".into(), service.into()),
                ("n".into(), sequence.to_string()),
            ],
        };
        Self {
            rows: vec![
                make(1, "INFO", QUIET, "ship"),
                make(3, "ERROR", LOUD, "idx"),
            ],
        }
    }
}

impl RowProvider for Records {
    fn page(&self, _view_id: &str, request: ViewportRequest) -> RowPage {
        RowPage {
            total: self.rows.len(),
            rows: self
                .rows
                .iter()
                .skip(request.start)
                .take(request.len)
                .cloned()
                .collect(),
        }
    }

    fn row_by_id(&self, _view_id: &str, id: &RowId) -> Option<DisplayRow> {
        self.rows.iter().find(|row| &row.id == id).cloned()
    }

    fn index_of_id(&self, _view_id: &str, id: &RowId) -> Option<usize> {
        self.rows.iter().position(|row| &row.id == id)
    }

    fn revision(&self, _view_id: &str) -> u64 {
        1
    }
}

fn app() -> (Records, App) {
    let mut app = App::new(
        vec![SourceItem {
            id: "api".into(),
            name: "api".into(),
            health: "ok".into(),
        }],
        vec![ViewItem {
            id: "view".into(),
            source_id: "api".into(),
            name: "view".into(),
        }],
        false,
    );
    let provider = Records::new();
    app.sync_provider(&provider, 8);
    (provider, app)
}

fn draw(provider: &Records, app: &mut App, theme: Theme) -> Buffer {
    draw_at(provider, app, theme, SIZE)
}

fn draw_at(provider: &Records, app: &mut App, theme: Theme, size: (u16, u16)) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(size.0, size.1)).expect("terminal");
    terminal
        .draw(|frame| ui::render_with_theme(frame, app, provider, theme, None))
        .expect("render");
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

fn line(buffer: &Buffer, y: u16) -> String {
    (0..buffer.area.width)
        .map(|x| buffer[(x, y)].symbol())
        .collect()
}

/// The per-cell styles of `needle`, on the first row that also contains
/// `within`. Two panes show the same record, so an unqualified search would
/// find whichever the layout puts first.
fn styles(buffer: &Buffer, within: &str, needle: &str) -> Vec<(Color, Color, Modifier)> {
    for y in 0..buffer.area.height {
        let text = line(buffer, y);
        if !text.contains(within) {
            continue;
        }
        let Some(byte) = text.find(needle) else {
            continue;
        };
        let start = text[..byte].chars().count() as u16;
        return (0..needle.chars().count() as u16)
            .map(|offset| {
                let cell = &buffer[(start + offset, y)];
                (cell.fg, cell.bg, cell.modifier)
            })
            .collect();
    }
    panic!(
        "no row containing {within:?} and {needle:?} in:\n{}",
        screen(buffer)
    );
}

/// Put the cursor on the first record, so the second renders in the log in its
/// own colours rather than in the selection highlight.
fn focus_quiet(provider: &Records, app: &mut App) {
    app.handle(Action::Top, provider);
    assert_eq!(
        app.view_state().and_then(|state| state.selected.clone()),
        Some(RowId::new("api", 1))
    );
}

/// Select the second record and open Details on it.
fn focus_loud(provider: &Records, app: &mut App) {
    app.handle(Action::Top, provider);
    app.handle(Action::MoveLine(1), provider);
    assert_eq!(
        app.view_state().and_then(|state| state.selected.clone()),
        Some(RowId::new("api", 3))
    );
    app.handle(Action::ToggleDetails, provider);
}

/// The property the user asked for: the same record, the same colours.
///
/// The comparison is against the log row while the cursor is *elsewhere*,
/// because the selection highlight is a cursor and not a colour of the record
/// — Details always shows the selected row, so it can never reproduce it.
#[test]
fn the_details_pane_colours_a_record_exactly_as_the_log_pane_does() {
    // Every depth, because the point of sharing the two functions is that a
    // new palette reaches both panes at once.
    for depth in [
        ColorDepth::TrueColor,
        ColorDepth::Indexed256,
        ColorDepth::Ansi16,
    ] {
        for theme in [
            Theme::LOVE_DARK.with_depth(depth),
            Theme::LOVE_LIGHT.with_depth(depth),
            Theme::TERMINAL.with_depth(depth),
        ] {
            let (provider, mut app) = app();
            // The cursor is on the first record, so the second renders in its
            // own colours in the log.
            focus_quiet(&provider, &mut app);
            let log = draw(&provider, &mut app, theme);
            let in_log = styles(&log, LOUD, LOUD);

            focus_loud(&provider, &mut app);
            let details = draw(&provider, &mut app, theme);
            let in_details = styles(&details, "raw: ", LOUD);

            assert_eq!(
                in_details,
                in_log,
                "{:?}/{depth:?}: the same record is coloured differently\n{}",
                theme.id,
                screen(&details)
            );
            // Not vacuous: the record really is coloured, key by key.
            assert!(
                in_log.iter().map(|(fg, _, _)| *fg).collect::<Vec<_>>()
                    != vec![in_log[0].0; in_log.len()],
                "{:?}/{depth:?}: the log row carried one flat colour",
                theme.id
            );
        }
    }
}

/// A column's name is the same colour wherever it is named: as a JSON key in
/// the log's raw line, and as the label of the pane's row for it.
///
/// The tree's cursor row is the exception, and the right one: it carries the
/// selection style exactly as the log's selected row does.
#[test]
fn a_column_name_carries_one_identity_colour_in_both_panes() {
    let theme = Theme::LOVE_DARK;
    let (provider, mut app) = app();
    focus_loud(&provider, &mut app);
    let details = draw(&provider, &mut app, theme);
    let rendered = screen(&details);

    // `raw` is a column, and so is every field row below the cursor.
    for column in ["raw", "n", "bad", "origin"] {
        let label = styles(&details, &format!("{column}: "), &format!("{column}:"));
        assert_eq!(
            label[0].0,
            theme.value_color(column),
            "{column} label\n{rendered}"
        );
    }
    // And that is the colour the same key carries inside the raw JSON line,
    // which is what makes the two panes agree rather than merely both be
    // colourful.
    for column in ["n", "bad"] {
        let key = styles(&details, "raw: ", &format!("\"{column}\""));
        assert_eq!(key[0].0, theme.value_color(column), "{rendered}");
    }

    // The cursor row is the tree's selection, so it wins over the record's
    // colours, exactly as the log's selected row does.
    let cursor = styles(&details, "svc: ", "svc:");
    assert_eq!(cursor[0].0, theme.selection_fg, "{rendered}");
    assert_eq!(cursor[0].1, theme.selection_bg, "{rendered}");
}

/// A tree scalar is the record's own bytes in the colour the log line gives the
/// same token.
#[test]
fn a_tree_scalar_matches_the_log_lines_token() {
    let theme = Theme::LOVE_DARK;
    let (provider, mut app) = app();
    focus_quiet(&provider, &mut app);
    let log = draw(&provider, &mut app, theme);
    let in_log = styles(&log, LOUD, "null");

    focus_loud(&provider, &mut app);
    let details = draw(&provider, &mut app, theme);
    let in_tree = styles(&details, "bad: ", "null");
    assert_eq!(
        in_tree,
        in_log,
        "a scalar must be the colour the line gives it\n{}",
        screen(&details)
    );
    assert_eq!(in_tree[0].0, theme.json.null, "{}", screen(&details));
}

/// The record's severity reaches the pane, and the pane is drawn on the
/// surface the identity colours were measured against.
#[test]
fn the_pane_carries_the_records_severity_on_the_workspace_surface() {
    let theme = Theme::LOVE_DARK;
    let (provider, mut app) = app();
    focus_loud(&provider, &mut app);
    let details = draw(&provider, &mut app, theme);

    // `fixture` is not JSON, so it keeps the record's row style — which for an
    // ERROR record is the severity colour, exactly as the log row's cells are.
    let value = styles(&details, "origin: ", "fixture");
    assert_eq!(
        value[0].0,
        theme.severity_color("ERROR").expect("ERROR is a severity"),
        "{}",
        screen(&details)
    );
    // The pane shares the log's surface. `Theme::value_color` lifts an identity
    // colour until it reads on `base_bg`; on any other background that
    // measurement is of something the user is not looking at.
    assert_eq!(value[0].1, theme.base_bg, "{}", screen(&details));
    assert_ne!(theme.base_bg, theme.dialog_bg, "the two surfaces differ");
}

/// The colour-field hash outranks severity in Details for the same reason it
/// does in the log: the user asked for that colouring explicitly.
#[test]
fn a_colour_field_outranks_severity_in_both_panes() {
    let theme = Theme::LOVE_DARK;
    let (provider, mut app) = app();
    focus_loud(&provider, &mut app);
    if let Some(state) = app.views.active_mut() {
        state.color_field = Some("svc".into());
    }
    let details = draw(&provider, &mut app, theme);
    let value = styles(&details, "origin: ", "fixture");
    assert_eq!(
        value[0].0,
        theme.value_color("idx"),
        "the pane must follow the view's colour field\n{}",
        screen(&details)
    );
    assert_ne!(value[0].0, theme.severity_color("ERROR").unwrap());
}

/// The narrow layout keeps the colouring it can show.
///
/// At 54x16 the pane has two content rows and the raw value wraps below them,
/// so only the label is on screen — that is the pre-existing narrow-layout
/// clip `dialog-design.md` records for this pane, and it is the same on `main`.
/// What this pins is that the colouring survives the narrow layout rather than
/// how much of the record fits in it.
#[test]
fn the_narrow_pane_keeps_the_colours_it_can_show() {
    let theme = Theme::LOVE_DARK;
    let (provider, mut app) = app();
    focus_loud(&provider, &mut app);
    let narrow = draw_at(&provider, &mut app, theme, (54, 16));
    let rendered = screen(&narrow);
    let label = styles(&narrow, "raw:", "raw:");
    assert_eq!(label[0].0, theme.value_color("raw"), "{rendered}");
    assert_eq!(label[0].1, theme.base_bg, "{rendered}");
}

/// A colour rule paints the record, not the pane: the log row and the Details
/// pane both take the rule's colour, because both resolve it through
/// `record_style`. Without that the two panes disagree the moment a rule is
/// applied — the bug this pane's own conversion existed to end.
#[test]
fn a_rule_that_paints_the_log_row_paints_the_details_pane_too() {
    for depth in [ColorDepth::TrueColor, ColorDepth::Indexed256] {
        let theme = Theme::LOVE_DARK.with_depth(depth);
        let mut provider = Records::new();
        // The engine reported that rule 1 matched the second record; the
        // terminal only looks the colour up.
        provider.rows[1]
            .details
            .push(("color_rule".into(), "1".into()));
        let mut app = App::new(
            vec![SourceItem {
                id: "api".into(),
                name: "api".into(),
                health: "ok".into(),
            }],
            vec![ViewItem {
                id: "view".into(),
                source_id: "api".into(),
                name: "view".into(),
            }],
            false,
        );
        app.sync_provider(&provider, 8);
        if let Some(state) = app.views.active_mut() {
            state.color_rules = vec![ColorRule {
                predicate: "idx".into(),
                color: RuleColor::Green,
            }];
        }
        focus_loud(&provider, &mut app);
        let buffer = draw(&provider, &mut app, theme);
        // Details shows the selected record; the log shows it selected too, so
        // the comparison is against the rule colour itself rather than against
        // the log row, which is wearing the cursor. The record's own colour is
        // read off a span the JSON lexer does not claim — inside the raw line
        // every token takes its own token colour in both panes.
        let rule = theme.rule_color(RuleColor::Green);
        let details = styles(&buffer, "stable display id", "api:3");
        assert!(
            details.iter().all(|(fg, _, _)| *fg == rule),
            "{depth:?}: Details ignored the rule: {details:?}\n{}",
            screen(&buffer)
        );
        // And it is the rule's colour rather than the severity red the record
        // would otherwise have taken.
        assert_ne!(
            Some(rule),
            theme.severity_color("ERROR"),
            "the fixture must distinguish the two"
        );
    }
}
