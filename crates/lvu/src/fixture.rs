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
    capture_time: HashMap<String, Option<crate::CaptureTimeRange>>,
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
    /// Identity and capture time of every visible row, in display order.
    fn visible_times(&self, view_id: &str) -> Vec<(RowId, i64)> {
        let data = self.data.lock().expect("fixture lock");
        let Some(visible) = data.visible.get(view_id) else {
            return Vec::new();
        };
        let Some(rows) = data.rows.get(view_id) else {
            return Vec::new();
        };
        visible
            .iter()
            .filter_map(|index| rows.get(*index))
            .filter_map(|row| {
                row.captured_at_unix_nanos
                    .map(|time| (row.id.clone(), time))
            })
            .collect()
    }

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
                    capture_time: HashMap::new(),
                    scheduled,
                    revisions: HashMap::from([("all".into(), 1), ("errors".into(), 1)]),
                    tick: 0,
                })),
            },
            sources,
            views,
        )
    }

    /// A stream with real quiet periods in it, for gap navigation.
    ///
    /// Records arrive in three bursts: a short one, then ten minutes of
    /// silence, then another, then half an hour of silence. That is the shape
    /// gap navigation exists to find, and the demo fixture — one record per
    /// second, forever — deliberately has none of it.
    pub fn gapped() -> (Self, Vec<SourceItem>, Vec<ViewItem>) {
        let sources = vec![SourceItem {
            id: "gaps".into(),
            name: "Gapped fixture".into(),
            health: "synthetic/static".into(),
        }];
        let views = vec![ViewItem {
            id: "all".into(),
            source_id: "gaps".into(),
            name: "All events".into(),
        }];
        // Seconds from the start of the stream. The two gaps are 600s and
        // 1800s, either side of the one-minute default threshold.
        const OFFSETS: [i64; 6] = [0, 1, 2, 602, 603, 2403];
        let rows: Vec<DisplayRow> = OFFSETS
            .iter()
            .enumerate()
            .map(|(index, offset)| {
                let mut value = row(
                    "gaps",
                    index as u64 + 1,
                    "INFO",
                    format!("gapped event {:02}", index + 1),
                );
                value.captured_at_unix_nanos = Some(offset * 1_000_000_000);
                value.timestamp = format!("+{offset:04}s");
                value
            })
            .collect();
        let visible = HashMap::from([("all".to_owned(), (0..rows.len()).collect())]);
        (
            Self {
                data: Arc::new(Mutex::new(FixtureData {
                    rows: HashMap::from([("all".to_owned(), rows)]),
                    visible,
                    search: HashMap::new(),
                    capture_time: HashMap::new(),
                    scheduled: HashMap::new(),
                    revisions: HashMap::from([("all".to_owned(), 1)]),
                    tick: 0,
                })),
            },
            sources,
            views,
        )
    }

    /// JSON-shaped fixture rows for the highlighting demo. Row 2 spells its key
    /// with a `\u005f` escape so decoded key identity can be checked against
    /// the plain `request_id` spelling, and carries a marker far enough right
    /// that horizontal panning has to move to reach it.
    pub fn json_demo() -> (Self, Vec<SourceItem>, Vec<ViewItem>) {
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
        let json_rows: Vec<DisplayRow> = (1..=32)
            .map(|sequence| DisplayRow {
                id: RowId::new("json", sequence),
                timestamp: format!("12:00:{sequence:02}"),
                captured_at_unix_nanos: Some(sequence as i64 * 1_000_000_000),
                level: if sequence % 7 == 0 { "WARN" } else { "INFO" }.into(),
                text: if sequence == 2 {
                    r#"{"request\u005fid":"same-東京","wide":"界界e\u0301","ok":true,"count":2,"none":null,"tail":"COPY_JSON_MARKER"}"#.into()
                } else {
                    format!(
                        r#"{{"request_id":"same-東京","wide":"界界e\u0301","ok":true,"count":{sequence},"none":null,"tail":"ROW_{sequence:02}_END"}}"#
                    )
                },
                details: Vec::new(),
                fields: vec![("request_id".into(), "same-東京".into())],
            })
            .collect();
        let rows: HashMap<String, Vec<DisplayRow>> = HashMap::from([("all".into(), json_rows)]);
        let visible = rows
            .iter()
            .map(|(view_id, rows)| (view_id.clone(), (0..rows.len()).collect()))
            .collect();
        (
            Self {
                data: Arc::new(Mutex::new(FixtureData {
                    rows,
                    visible,
                    search: HashMap::new(),
                    capture_time: HashMap::new(),
                    scheduled: HashMap::new(),
                    revisions: HashMap::from([("all".into(), 1)]),
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

    /// Gives a newly derived view the rows of the view it came from.
    ///
    /// Mirrors what registering a derived view does in the real runtime, so
    /// tests can exercise a fork against a view that actually has content.
    pub fn derive_view(&self, origin: &str, view_id: &str) {
        let mut data = self.data.lock().expect("fixture lock");
        let Some(rows) = data.rows.get(origin).cloned() else {
            return;
        };
        let visible = data.visible.get(origin).cloned().unwrap_or_default();
        data.rows.insert(view_id.to_owned(), rows);
        data.visible.insert(view_id.to_owned(), visible);
        *data.revisions.entry(view_id.to_owned()).or_default() += 1;
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
                let matches = matches
                    && data
                        .capture_time
                        .get(&view_id)
                        .copied()
                        .flatten()
                        .is_none_or(|window| {
                            row.captured_at_unix_nanos.is_some_and(|timestamp| {
                                timestamp >= window.start_unix_nanos
                                    && timestamp < window.end_unix_nanos
                            })
                        });
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
    /// The visible rows' capture timestamps, in display order. The fixture is
    /// small by construction, so it answers exactly rather than approximately;
    /// the bound the real engine obeys is the engine's concern. Every fixture
    /// row carries a capture time and nothing else, so the basis is ignored.
    fn time_bounds(&self, view_id: &str, _basis: crate::TimeBasis) -> Option<crate::TimeBounds> {
        let times = self.visible_times(view_id);
        let first = times.iter().map(|(_, time)| *time).min()?;
        let last = times.iter().map(|(_, time)| *time).max()?;
        Some(crate::TimeBounds {
            first_unix_nanos: first,
            last_unix_nanos: last,
            count: times.len(),
            missing: 0,
        })
    }

    fn find_gap(
        &self,
        view_id: &str,
        from: Option<&RowId>,
        direction: crate::GapDirection,
        threshold_nanos: i64,
        _basis: crate::TimeBasis,
    ) -> Option<crate::GapHit> {
        if threshold_nanos <= 0 {
            return None;
        }
        let times = self.visible_times(view_id);
        if times.len() < 2 {
            return None;
        }
        let start = from
            .and_then(|row| times.iter().position(|(id, _)| id == row))
            .unwrap_or(match direction {
                crate::GapDirection::Forward => 0,
                crate::GapDirection::Backward => times.len(),
            });
        let exceeds =
            |index: usize| times[index].1.saturating_sub(times[index - 1].1) > threshold_nanos;
        let hit = |index: usize| crate::GapHit {
            row: times[index].0.clone(),
            gap_nanos: times[index].1.saturating_sub(times[index - 1].1),
            previous_unix_nanos: times[index - 1].1,
            previous_row: times[index - 1].0.clone(),
        };
        match direction {
            crate::GapDirection::Forward => (start + 1..times.len())
                .find(|index| exceeds(*index))
                .map(hit),
            crate::GapDirection::Backward => (1..start.min(times.len()))
                .rev()
                .find(|index| exceeds(*index))
                .map(hit),
        }
    }

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

    fn context_page(
        &self,
        view_id: &str,
        anchor: &RowId,
        offset: isize,
        len: usize,
    ) -> crate::ContextPage {
        let data = self.data.lock().expect("fixture lock");
        let mut result = crate::ContextPage {
            anchor_position: None,
            start: 0,
            total: 0,
            rows: Vec::new(),
            pending: false,
            diagnostic: None,
        };
        let Some(rows) = data.rows.get(view_id) else {
            return result;
        };
        let Some(position) = rows.iter().position(|row| &row.id == anchor) else {
            return result;
        };
        result.anchor_position = Some(position);
        result.total = rows.len();
        result.start = position
            .saturating_add_signed(offset)
            .min(rows.len().saturating_sub(1));
        result.rows = rows
            .iter()
            .skip(result.start)
            .take(len.min(32))
            .filter(|row| row.id.source_id == anchor.source_id)
            .cloned()
            .collect();
        result
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
            apply_fixture_search(
                &self.data,
                &request.view_id,
                literal,
                request.constraints.capture_time,
            );
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

fn apply_fixture_search(
    data: &Mutex<FixtureData>,
    view_id: &str,
    literal: &str,
    capture_time: Option<crate::CaptureTimeRange>,
) {
    let mut data = data.lock().expect("fixture lock");
    let visible = data
        .rows
        .get(view_id)
        .map(|rows| {
            rows.iter()
                .enumerate()
                .filter(|(_, row)| {
                    matches_literal(&row.text, literal)
                        && capture_time.is_none_or(|window| {
                            row.captured_at_unix_nanos.is_some_and(|timestamp| {
                                timestamp >= window.start_unix_nanos
                                    && timestamp < window.end_unix_nanos
                            })
                        })
                })
                .map(|(index, _)| index)
                .collect()
        })
        .unwrap_or_default();
    data.search.insert(view_id.to_owned(), literal.to_owned());
    data.capture_time.insert(view_id.to_owned(), capture_time);
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
        captured_at_unix_nanos: Some(sequence as i64 * 1_000_000_000),
        level: level.into(),
        details: vec![
            ("fixture".into(), "true (not captured data)".into()),
            ("record_id".into(), format!("{source}:{sequence}")),
            ("message".into(), text.clone()),
            (
                "event_time_utc_nanos".into(),
                (100_000_000_000_i64 + sequence as i64 * 1_000_000_000).to_string(),
            ),
        ],
        fields: vec![
            ("service".into(), source.into()),
            ("level".into(), level.into()),
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
