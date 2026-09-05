use std::collections::HashMap;

use crate::{
    app::{SourceItem, ViewItem},
    provider::{DisplayRow, RowId, RowPage, RowProvider, ViewportRequest},
};

#[derive(Clone, Debug)]
struct ScheduledRow {
    tick: u64,
    row: DisplayRow,
}

/// Deterministic provider used only by the demo binary and UI tests. It is not
/// acquisition and does not persist raw records.
pub struct FixtureProvider {
    rows: HashMap<String, Vec<DisplayRow>>,
    scheduled: HashMap<String, Vec<ScheduledRow>>,
    revisions: HashMap<String, u64>,
    tick: u64,
}

impl FixtureProvider {
    pub fn demo() -> (Self, Vec<SourceItem>, Vec<ViewItem>) {
        let sources = vec![
            SourceItem {
                id: "api".into(),
                name: "API fixture".into(),
                health: "synthetic/live".into(),
            },
            SourceItem {
                id: "worker".into(),
                name: "Worker fixture".into(),
                health: "synthetic/static".into(),
            },
        ];
        let views = vec![
            ViewItem {
                id: "all".into(),
                source_id: "api".into(),
                name: "All events".into(),
            },
            ViewItem {
                id: "errors".into(),
                source_id: "worker".into(),
                name: "Errors only".into(),
            },
        ];
        let mut rows = HashMap::new();
        rows.insert(
            "all".into(),
            (1..=16)
                .map(|sequence| {
                    row(
                        "api",
                        sequence,
                        if sequence % 5 == 0 { "WARN" } else { "INFO" },
                        if sequence == 3 {
                            "Unicode 東京 café e\u{301} request completed".into()
                        } else {
                            format!("fixture request {sequence:02} completed")
                        },
                    )
                })
                .collect(),
        );
        rows.insert(
            "errors".into(),
            vec![
                row("worker", 4, "ERROR", "fixture queue unavailable".into()),
                row("worker", 9, "ERROR", "fixture retry exhausted".into()),
                row("worker", 12, "ERROR", "fixture job failed safely".into()),
            ],
        );
        let mut scheduled = HashMap::new();
        scheduled.insert(
            "all".into(),
            vec![
                ScheduledRow {
                    tick: 1,
                    row: row("api", 17, "INFO", "late fixture arrival 17".into()),
                },
                ScheduledRow {
                    tick: 2,
                    row: row("api", 18, "WARN", "late fixture arrival 18".into()),
                },
            ],
        );
        (
            Self {
                rows,
                scheduled,
                revisions: HashMap::from([("all".into(), 1), ("errors".into(), 1)]),
                tick: 0,
            },
            sources,
            views,
        )
    }

    pub fn advance(&mut self) -> bool {
        self.tick += 1;
        let mut changed = false;
        for (view_id, pending) in &mut self.scheduled {
            let split = pending.partition_point(|item| item.tick <= self.tick);
            if split > 0 {
                let ready: Vec<_> = pending.drain(..split).map(|item| item.row).collect();
                self.rows.entry(view_id.clone()).or_default().extend(ready);
                *self.revisions.entry(view_id.clone()).or_default() += 1;
                changed = true;
            }
        }
        changed
    }
}

impl RowProvider for FixtureProvider {
    fn page(&self, view_id: &str, request: ViewportRequest) -> RowPage {
        let rows = self.rows.get(view_id).map_or(&[][..], Vec::as_slice);
        let start = request.start.min(rows.len());
        let end = start.saturating_add(request.len).min(rows.len());
        RowPage {
            total: rows.len(),
            rows: rows[start..end].to_vec(),
        }
    }

    fn revision(&self, view_id: &str) -> u64 {
        self.revisions.get(view_id).copied().unwrap_or_default()
    }

    fn row_by_id(&self, view_id: &str, id: &RowId) -> Option<DisplayRow> {
        self.rows
            .get(view_id)?
            .iter()
            .find(|row| &row.id == id)
            .cloned()
    }

    fn index_of_id(&self, view_id: &str, id: &RowId) -> Option<usize> {
        self.rows.get(view_id)?.iter().position(|row| &row.id == id)
    }
}

fn row(source: &str, sequence: u64, level: &str, text: String) -> DisplayRow {
    DisplayRow {
        id: RowId::new(source, sequence),
        timestamp: format!("12:00:{sequence:02}"),
        level: level.into(),
        details: vec![
            ("fixture".into(), "true (not captured data)".into()),
            ("record_id".into(), format!("{source}:{sequence}")),
            ("message".into(), text.clone()),
        ],
        text,
    }
}
