use lvu::{
    App, ContextPage, DisplayRow, RowId, RowPage, RowProvider, SourceItem, ViewItem,
    terminal::{self, UnwiredQueryDispatcher},
    theme::ThemeId,
};
use std::process::ExitCode;

struct JsonFixture {
    rows: Vec<DisplayRow>,
}

impl RowProvider for JsonFixture {
    fn page(&self, _view_id: &str, request: lvu::ViewportRequest) -> RowPage {
        let start = request.start.min(self.rows.len());
        let end = start.saturating_add(request.len).min(self.rows.len());
        RowPage {
            total: self.rows.len(),
            rows: self.rows[start..end].to_vec(),
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

    fn context_page(
        &self,
        _view_id: &str,
        _anchor: &RowId,
        _offset: isize,
        _len: usize,
    ) -> ContextPage {
        ContextPage {
            anchor_position: None,
            start: 0,
            total: 0,
            rows: Vec::new(),
            pending: false,
            diagnostic: None,
        }
    }
}

fn main() -> ExitCode {
    let rows = (1..=32)
        .map(|sequence| DisplayRow {
            id: RowId::new("json", sequence),
            timestamp: format!("12:00:{sequence:02}"),
            captured_at_unix_nanos: Some(sequence as i64),
            level: if sequence % 7 == 0 { "WARN" } else { "INFO" }.into(),
            text: if sequence == 2 {
                r#"{"request\u005fid":"same-東京","wide":"界界e\u0301","ok":true,"count":2,"none":null,"tail":"COPY_JSON_MARKER"}"#.into()
            } else {
                format!(r#"{{"request_id":"same-東京","wide":"界界e\u0301","ok":true,"count":{sequence},"none":null,"tail":"ROW_{sequence:02}_END"}}"#)
            },
            details: Vec::new(),
            fields: vec![("request_id".into(), "same-東京".into())],
        })
        .collect();
    let mut provider = JsonFixture { rows };
    let sources = vec![SourceItem {
        id: "json".into(),
        name: "JSON fixture".into(),
        health: "synthetic/static".into(),
    }];
    let views = vec![ViewItem {
        id: "all".into(),
        source_id: "json".into(),
        name: "JSON events".into(),
    }];
    let mut app = App::new(sources, views, true);
    app.theme_id = ThemeId::LoveDark;
    let mut dispatcher = UnwiredQueryDispatcher::new();
    match terminal::run(app, &mut provider, &mut dispatcher, |_| false) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("json fixture: {error}");
            ExitCode::FAILURE
        }
    }
}
