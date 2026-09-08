use lvu::{
    Action, App, DisplayRow, RowId, RowPage, RowProvider, ViewportRequest,
    app::{SourceItem, ViewItem},
    theme::Theme,
    ui,
};
use ratatui::{Terminal, backend::TestBackend};

struct WrappedJson(DisplayRow);

impl RowProvider for WrappedJson {
    fn page(&self, _view_id: &str, request: ViewportRequest) -> RowPage {
        RowPage {
            total: 1,
            rows: (request.start == 0 && request.len > 0)
                .then(|| self.0.clone())
                .into_iter()
                .collect(),
        }
    }

    fn row_by_id(&self, _view_id: &str, id: &RowId) -> Option<DisplayRow> {
        (&self.0.id == id).then(|| self.0.clone())
    }

    fn index_of_id(&self, _view_id: &str, id: &RowId) -> Option<usize> {
        (&self.0.id == id).then_some(0)
    }

    fn revision(&self, _view_id: &str) -> u64 {
        1
    }
}

fn screen(app: &mut App, provider: &WrappedJson) -> String {
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal
        .draw(|frame| ui::render_with_theme(frame, app, provider, Theme::LOVE_DARK, None))
        .unwrap();
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn ansi_wrapped_json_is_clean_but_does_not_invent_a_navigable_tree() {
    let provider = WrappedJson(DisplayRow {
        id: RowId::new("source", 1),
        timestamp: "now".into(),
        captured_at_unix_nanos: None,
        level: "INFO".into(),
        text: "\u{1b}[2m{\"outer\":{\"value\":1},\"literal\":\"[2m\"}\u{1b}[0m".into(),
        details: Vec::new(),
        fields: Vec::new(),
    });
    let mut app = App::new(
        vec![SourceItem {
            id: "source".into(),
            name: "source".into(),
            health: "ok".into(),
        }],
        vec![ViewItem {
            id: "view".into(),
            source_id: "source".into(),
            name: "view".into(),
        }],
        false,
    );
    app.sync_provider(&provider, 8);
    app.handle(Action::ToggleDetails, &provider);

    assert!(app.details_rows(&provider).is_empty());
    let before = screen(&mut app, &provider);
    assert!(before.contains(r#"raw: {"outer":{"value":1},"literal":"[2m"}"#));
    assert!(!before.contains("[0m") && !before.contains("[2m{"));

    app.handle(Action::DetailsPath(None), &provider);
    assert!(app.details_rows(&provider).is_empty());
    assert_eq!(screen(&mut app, &provider), before);
}
