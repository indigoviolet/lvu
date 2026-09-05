use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
};

use crate::{
    app::{QueryCompletion, QueryFailure, QueryPurpose, QueryRequest, SourceItem, ViewItem},
    provider::{DisplayRow, RowId, RowPage, RowProvider, ViewportRequest},
    terminal::QueryDispatcher,
};

const MAX_FIXTURE_REQUESTS: usize = 32;

#[derive(Clone, Debug)]
struct ScheduledRow {
    tick: u64,
    row: DisplayRow,
}

struct FixtureData {
    rows: HashMap<String, Vec<DisplayRow>>,
    visible: HashMap<String, Vec<usize>>,
    search: HashMap<String, String>,
    scheduled: HashMap<String, Vec<ScheduledRow>>,
    revisions: HashMap<String, u64>,
    tick: u64,
}

/// Deterministic provider used only by the demo binary and UI tests. It is not
/// acquisition and does not persist raw records.
#[derive(Clone)]
pub struct FixtureProvider {
    data: Arc<Mutex<FixtureData>>,
}

pub struct FixtureQueryDispatcher {
    data: Arc<Mutex<FixtureData>>,
    pending: VecDeque<QueryRequest>,
    latest_revision: HashMap<String, u64>,
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
        let rows: HashMap<String, Vec<DisplayRow>> = HashMap::from([
            (
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
            ),
            (
                "errors".into(),
                vec![
                    row("worker", 4, "ERROR", "fixture queue unavailable".into()),
                    row("worker", 9, "ERROR", "fixture retry exhausted".into()),
                    row("worker", 12, "ERROR", "fixture job failed safely".into()),
                ],
            ),
        ]);
        let visible = rows
            .iter()
            .map(|(view_id, rows)| (view_id.clone(), (0..rows.len()).collect()))
            .collect();
        let scheduled = HashMap::from([(
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
        )]);
        (
            Self {
                data: Arc::new(Mutex::new(FixtureData {
                    rows,
                    visible,
                    search: HashMap::new(),
                    scheduled,
                    revisions: HashMap::from([("all".into(), 1), ("errors".into(), 1)]),
                    tick: 0,
                })),
            },
            sources,
            views,
        )
    }

    pub fn query_dispatcher(&self) -> FixtureQueryDispatcher {
        FixtureQueryDispatcher {
            data: Arc::clone(&self.data),
            pending: VecDeque::new(),
            latest_revision: HashMap::new(),
        }
    }

    pub fn advance(&mut self) -> bool {
        let mut data = self.data.lock().expect("fixture lock");
        data.tick += 1;
        let tick = data.tick;
        let view_ids: Vec<String> = data.scheduled.keys().cloned().collect();
        let mut changed = false;
        for view_id in view_ids {
            let ready = {
                let pending = data.scheduled.get_mut(&view_id).expect("scheduled view");
                let split = pending.partition_point(|item| item.tick <= tick);
                pending
                    .drain(..split)
                    .map(|item| item.row)
                    .collect::<Vec<_>>()
            };
            for row in ready {
                let matches = matches_literal(
                    &row.text,
                    data.search.get(&view_id).map_or("", String::as_str),
                );
                let index = data.rows.entry(view_id.clone()).or_default().len();
                data.rows.get_mut(&view_id).expect("fixture rows").push(row);
                if matches {
                    data.visible.entry(view_id.clone()).or_default().push(index);
                }
                *data.revisions.entry(view_id.clone()).or_default() += 1;
                changed = true;
            }
        }
        changed
    }
}

impl RowProvider for FixtureProvider {
    fn page(&self, view_id: &str, request: ViewportRequest) -> RowPage {
        let data = self.data.lock().expect("fixture lock");
        let empty = Vec::new();
        let visible = data.visible.get(view_id).unwrap_or(&empty);
        let start = request.start.min(visible.len());
        let end = start.saturating_add(request.len).min(visible.len());
        let rows = data.rows.get(view_id);
        RowPage {
            total: visible.len(),
            rows: visible[start..end]
                .iter()
                .filter_map(|index| rows.and_then(|rows| rows.get(*index)).cloned())
                .collect(),
        }
    }

    fn revision(&self, view_id: &str) -> u64 {
        self.data
            .lock()
            .expect("fixture lock")
            .revisions
            .get(view_id)
            .copied()
            .unwrap_or_default()
    }

    fn row_by_id(&self, view_id: &str, id: &RowId) -> Option<DisplayRow> {
        self.data
            .lock()
            .expect("fixture lock")
            .rows
            .get(view_id)?
            .iter()
            .find(|row| &row.id == id)
            .cloned()
    }

    fn index_of_id(&self, view_id: &str, id: &RowId) -> Option<usize> {
        let data = self.data.lock().expect("fixture lock");
        let rows = data.rows.get(view_id)?;
        data.visible
            .get(view_id)?
            .iter()
            .position(|index| rows.get(*index).is_some_and(|row| &row.id == id))
    }
}

impl QueryDispatcher for FixtureQueryDispatcher {
    fn submit(&mut self, request: QueryRequest) -> Result<(), String> {
        self.latest_revision
            .entry(request.view_id.clone())
            .and_modify(|revision| *revision = (*revision).max(request.revision))
            .or_insert(request.revision);
        if let Some(existing) = self
            .pending
            .iter_mut()
            .find(|queued| queued.view_id == request.view_id && queued.purpose == request.purpose)
        {
            *existing = request;
            return Ok(());
        }
        if self.pending.len() >= MAX_FIXTURE_REQUESTS {
            return Err("fixture query queue is full".into());
        }
        self.pending.push_back(request);
        Ok(())
    }

    fn poll(&mut self) -> Option<QueryCompletion> {
        let request = self.pending.pop_front()?;
        if self.latest_revision.get(&request.view_id) != Some(&request.revision) {
            return Some(QueryCompletion {
                view_id: request.view_id,
                generation: request.generation,
                revision: request.revision,
                purpose: request.purpose,
                result: Ok(()),
            });
        }
        let result = if request.constraints.advanced_polars.is_some() {
            Err(QueryFailure {
                purpose: QueryPurpose::Advanced,
                message:
                    "advanced Polars adapter is not wired in demo mode; applied filter is unchanged"
                        .into(),
            })
        } else {
            let literal = request
                .constraints
                .text
                .as_ref()
                .map_or("", |constraint| constraint.literal.as_str());
            apply_fixture_search(&self.data, &request.view_id, literal);
            Ok(())
        };
        Some(QueryCompletion {
            view_id: request.view_id,
            generation: request.generation,
            revision: request.revision,
            purpose: request.purpose,
            result,
        })
    }
}

fn apply_fixture_search(data: &Mutex<FixtureData>, view_id: &str, literal: &str) {
    let mut data = data.lock().expect("fixture lock");
    let visible = data
        .rows
        .get(view_id)
        .map(|rows| {
            rows.iter()
                .enumerate()
                .filter(|(_, row)| matches_literal(&row.text, literal))
                .map(|(index, _)| index)
                .collect()
        })
        .unwrap_or_default();
    data.search.insert(view_id.to_owned(), literal.to_owned());
    data.visible.insert(view_id.to_owned(), visible);
    *data.revisions.entry(view_id.to_owned()).or_default() += 1;
}

/// Locale-neutral Rust Unicode lowercase mapping followed by substring match.
fn matches_literal(message: &str, literal: &str) -> bool {
    literal.is_empty() || message.to_lowercase().contains(&literal.to_lowercase())
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

#[cfg(test)]
mod tests {
    use super::matches_literal;

    #[test]
    fn literal_search_handles_punctuation_case_empty_and_unicode() {
        assert!(matches_literal("hello.world [x] \"quoted\"", "."));
        assert!(matches_literal("hello.world [x] \"quoted\"", "["));
        assert!(matches_literal("hello.world [x] \"quoted\"", "\"quoted\""));
        assert!(matches_literal("Request COMPLETED", "completed"));
        assert!(matches_literal("Unicode 東京 CAFÉ", "東京 café"));
        assert!(matches_literal("anything", ""));
        assert!(!matches_literal("literal dotless", "."));
    }
}
