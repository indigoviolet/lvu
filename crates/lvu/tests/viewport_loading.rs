use std::cell::Cell;

use lvu::{
    App, DisplayRow, RowId, RowPage, RowProvider, ViewportRequest,
    app::{SourceItem, ViewItem},
    ui,
};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

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

fn render<P: RowProvider>(provider: &P, app: &mut App, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|frame| ui::render(frame, app, provider))
        .expect("render");
    screen(terminal.backend().buffer())
}

struct LoadingProvider {
    rows: Vec<DisplayRow>,
    blank: Cell<bool>,
    revision: Cell<u64>,
}

impl RowProvider for LoadingProvider {
    fn page(&self, _: &str, request: ViewportRequest) -> RowPage {
        if request.len == 0 {
            return RowPage {
                total: self.rows.len(),
                rows: vec![],
            };
        }
        if self.blank.get() {
            return RowPage {
                total: self.rows.len(),
                rows: vec![],
            };
        }
        let start = request.start.min(self.rows.len());
        let end = start.saturating_add(request.len).min(self.rows.len());
        RowPage {
            total: self.rows.len(),
            rows: self.rows[start..end].to_vec(),
        }
    }

    fn row_by_id(&self, _: &str, id: &RowId) -> Option<DisplayRow> {
        self.rows.iter().find(|row| &row.id == id).cloned()
    }

    fn index_of_id(&self, _: &str, id: &RowId) -> Option<usize> {
        self.rows.iter().position(|row| &row.id == id)
    }

    fn revision(&self, _: &str) -> u64 {
        self.revision.get()
    }
}

fn demo_rows() -> Vec<DisplayRow> {
    (0..12)
        .map(|sequence| DisplayRow {
            id: RowId::new("source", sequence),
            timestamp: String::new(),
            captured_at_unix_nanos: None,
            level: String::new(),
            text: format!("row {sequence}"),
            details: vec![],
            fields: vec![],
        })
        .collect()
}

fn demo_app() -> App {
    App::new(
        vec![SourceItem {
            id: "source".into(),
            name: "source".into(),
            health: "ok".into(),
        }],
        vec![ViewItem {
            id: "view".into(),
            source_id: "source".into(),
            name: "All".into(),
        }],
        true,
    )
}

#[test]
fn blank_over_indexed_rows_names_loading_without_moving_range() {
    let provider = LoadingProvider {
        rows: demo_rows(),
        blank: Cell::new(true),
        revision: Cell::new(1),
    };
    let mut app = demo_app();
    app.sync_provider(&provider, 4);
    let state = app.view_state().unwrap();
    assert_eq!(state.rows_drawn, 0);
    assert_eq!(state.last_total, 12);
    let shown = render(&provider, &mut app, 160, 20);
    assert!(shown.contains("0-0/12"), "{shown}");
    assert!(shown.contains("loading"), "{shown}");
}

#[test]
fn served_rows_clear_loading_and_keep_range() {
    let provider = LoadingProvider {
        rows: demo_rows(),
        blank: Cell::new(true),
        revision: Cell::new(1),
    };
    let mut app = demo_app();
    assert!(render(&provider, &mut app, 160, 20).contains("loading"));

    // Rendering re-syncs with the real log height (which fits all 12 rows),
    // so FOLLOW draws the whole stream from the top once rows are servable.
    provider.blank.set(false);
    provider.revision.set(2);
    let shown = render(&provider, &mut app, 160, 20);
    let state = app.view_state().unwrap();
    assert_eq!((state.top, state.rows_drawn), (0, 12));
    assert!(shown.contains("1-12/12"), "{shown}");
    assert!(!shown.contains("loading"), "{shown}");
}
