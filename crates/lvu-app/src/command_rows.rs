//! Read-only command results never participate in native membership predicates.
use lvu::{ContextPage, DisplayRow, RowId, RowPage, RowProvider, ViewportRequest};
use lvu_core::RecordId;
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Mutex},
};

const MAX_PUBLICATION_BYTES: usize = 1024 * 1024;
const MAX_ALL_PUBLICATION_BYTES: usize = 8 * MAX_PUBLICATION_BYTES;
const MAX_PUBLICATION_RECORDS: usize = 1024;

#[derive(Clone, Debug)]
pub struct CommandResult {
    pub fields: BTreeMap<String, Value>,
    pub diagnostic: Option<String>,
}

pub type CommandResults = BTreeMap<RecordId, CommandResult>;

struct Publication {
    rows: HashMap<RowId, CommandResult>,
    bytes: usize,
}

#[derive(Default)]
struct Shared {
    publications: HashMap<String, Publication>,
    configured: HashMap<String, bool>,
    revision: u64,
}

#[derive(Clone, Default)]
pub struct CommandPresentation(Arc<Mutex<Shared>>);

impl CommandPresentation {
    fn publication_bytes(rows: &CommandResults) -> Result<usize, String> {
        if rows.len() > MAX_PUBLICATION_RECORDS {
            return Err("command result exceeds 1024 records; narrow the view".into());
        }
        let mut bytes = 0usize;
        for row in rows.values() {
            bytes = bytes
                .saturating_add(
                    serde_json::to_vec(&row.fields)
                        .map_err(|e| e.to_string())?
                        .len(),
                )
                .saturating_add(row.diagnostic.as_ref().map_or(0, String::len));
            if bytes > MAX_PUBLICATION_BYTES {
                return Err("command result exceeds the 1 MiB publication limit".into());
            }
        }
        Ok(bytes)
    }

    fn check_capacity(shared: &Shared, view: &str, bytes: usize) -> Result<(), String> {
        let other_bytes: usize = shared
            .publications
            .iter()
            .filter(|(id, _)| id.as_str() != view)
            .map(|(_, p)| p.bytes)
            .sum();
        if other_bytes.saturating_add(bytes) > MAX_ALL_PUBLICATION_BYTES {
            return Err("command results exceed the 8 MiB workspace presentation limit".into());
        }
        Ok(())
    }

    /// The controller serializes publication/restore while a durable save is pending.
    pub fn can_publish(&self, view: &str, rows: &CommandResults) -> Result<(), String> {
        let bytes = Self::publication_bytes(rows)?;
        Self::check_capacity(
            &self.0.lock().expect("command presentation poisoned"),
            view,
            bytes,
        )
    }

    /// Admission is atomic: failure leaves the entire previous publication intact.
    pub fn publish(&self, view: &str, rows: CommandResults) -> Result<(), String> {
        let bytes = Self::publication_bytes(&rows)?;
        let mut shared = self.0.lock().expect("command presentation poisoned");
        Self::check_capacity(&shared, view, bytes)?;
        shared.publications.insert(
            view.into(),
            Publication {
                rows: rows
                    .into_iter()
                    .map(|(id, result)| {
                        (RowId::new(id.source_id.0.to_string(), id.sequence), result)
                    })
                    .collect(),
                bytes,
            },
        );
        shared.revision = shared.revision.wrapping_add(1);
        Ok(())
    }

    pub fn configure(&self, views: impl IntoIterator<Item = (String, bool)>) {
        let next: HashMap<_, _> = views.into_iter().collect();
        let mut shared = self.0.lock().expect("command presentation poisoned");
        if shared.configured != next {
            shared.publications.retain(|id, _| next.contains_key(id));
            shared.configured = next;
            shared.revision = shared.revision.wrapping_add(1);
        }
    }

    fn decorate(&self, view: &str, row: &mut DisplayRow) {
        let shared = self.0.lock().expect("command presentation poisoned");
        if let Some(result) = shared
            .publications
            .get(view)
            .and_then(|p| p.rows.get(&row.id))
        {
            row.details
                .push(("command.status".into(), "Ready · last explicit run".into()));
            for (key, value) in &result.fields {
                row.details.push((
                    format!("command.{}", display_text(key)),
                    match value {
                        Value::String(text) => display_text(text),
                        other => other.to_string(),
                    },
                ));
            }
            if let Some(diagnostic) = &result.diagnostic {
                row.details
                    .push(("command.diagnostic".into(), display_text(diagnostic)));
            }
        } else if shared.configured.get(view) == Some(&true) {
            row.details
                .push(("command.status".into(), "Pending — run explicitly".into()));
        }
    }
}

fn display_text(value: &str) -> String {
    let mut text = String::with_capacity(value.len());
    for character in value.chars() {
        if character.is_control() {
            text.extend(character.escape_default());
        } else {
            text.push(character);
        }
    }
    text
}

pub struct CommandRows<P> {
    pub native: P,
    pub presentation: CommandPresentation,
}

impl<P: RowProvider> RowProvider for CommandRows<P> {
    fn page(&self, view: &str, request: ViewportRequest) -> RowPage {
        let mut page = self.native.page(view, request);
        for row in &mut page.rows {
            self.presentation.decorate(view, row);
        }
        page
    }
    fn row_by_id(&self, view: &str, id: &RowId) -> Option<DisplayRow> {
        self.native.row_by_id(view, id).map(|mut row| {
            self.presentation.decorate(view, &mut row);
            row
        })
    }
    fn index_of_id(&self, view: &str, id: &RowId) -> Option<usize> {
        self.native.index_of_id(view, id)
    }
    fn revision(&self, view: &str) -> u64 {
        self.native.revision(view).wrapping_add(
            self.presentation
                .0
                .lock()
                .expect("command presentation poisoned")
                .revision,
        )
    }
    fn context_page(&self, view: &str, anchor: &RowId, offset: isize, len: usize) -> ContextPage {
        self.native.context_page(view, anchor, offset, len)
    }
    fn set_fold(&self, view: &str, request: &lvu::FoldRequest) {
        self.native.set_fold(view, request);
    }
    fn fold_summary(&self, view: &str) -> Option<lvu::FoldSummary> {
        self.native.fold_summary(view)
    }
    fn fold_members(&self, view: &str, id: &RowId) -> Vec<RowId> {
        self.native.fold_members(view, id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lvu_core::SourceId;
    use serde_json::json;

    struct NativeRows(Vec<DisplayRow>);
    impl RowProvider for NativeRows {
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
            9
        }
        fn context_page(&self, _: &str, _: &RowId, _: isize, len: usize) -> ContextPage {
            ContextPage {
                anchor_position: Some(0),
                start: 0,
                total: self.0.len(),
                rows: self.0.iter().take(len).cloned().collect(),
                pending: false,
                diagnostic: None,
            }
        }
    }

    fn fixture() -> (RecordId, NativeRows) {
        let id = RecordId {
            source_id: SourceId::new(),
            sequence: 10,
        };
        let rows = (10..12)
            .map(|sequence| DisplayRow {
                id: RowId::new(id.source_id.0.to_string(), sequence),
                timestamp: "unchanged".into(),
                captured_at_unix_nanos: Some(55),
                level: "INFO".into(),
                text: format!("original {sequence}"),
                details: vec![("raw".into(), format!("original {sequence}"))],
                fields: vec![("native".into(), "7".into())],
            })
            .collect();
        (id, NativeRows(rows))
    }

    #[test]
    fn results_decorate_details_without_changing_native_fields_membership_or_raw_context() {
        let (id, native) = fixture();
        let original = native.0.clone();
        let presentation = CommandPresentation::default();
        presentation.configure([("view".into(), true)]);
        let rows = CommandRows {
            native,
            presentation: presentation.clone(),
        };
        let before = rows.revision("view");
        presentation
            .publish(
                "view",
                BTreeMap::from([(
                    id,
                    CommandResult {
                        fields: BTreeMap::from([
                            ("answer".into(), json!(u64::MAX)),
                            ("text".into(), json!("界\u{1b}[2J")),
                        ]),
                        diagnostic: Some("note\nnext".into()),
                    },
                )]),
            )
            .unwrap();
        let page = rows.page("view", ViewportRequest { start: 0, len: 2 });
        assert_eq!(page.total, 2);
        assert_eq!(page.rows[0].text, original[0].text);
        assert_eq!(page.rows[0].fields, original[0].fields);
        assert!(
            page.rows[0]
                .details
                .contains(&("command.answer".into(), u64::MAX.to_string()))
        );
        assert!(
            page.rows[0]
                .details
                .contains(&("command.text".into(), "界\\u{1b}[2J".into()))
        );
        assert!(
            page.rows[1]
                .details
                .iter()
                .any(|(key, value)| key == "command.status" && value.contains("Pending"))
        );
        assert_ne!(rows.revision("view"), before);
        assert_eq!(rows.index_of_id("view", &original[1].id), Some(1));
        assert_eq!(
            rows.context_page("view", &original[0].id, 0, 2).rows,
            original
        );
        let prior = rows.row_by_id("view", &page.rows[0].id).unwrap();
        let too_big = BTreeMap::from([(
            id,
            CommandResult {
                fields: BTreeMap::from([("huge".into(), json!("x".repeat(MAX_PUBLICATION_BYTES)))]),
                diagnostic: None,
            },
        )]);
        assert!(presentation.can_publish("view", &too_big).is_err());
        assert!(presentation.publish("view", too_big).is_err());
        assert_eq!(rows.row_by_id("view", &prior.id).unwrap(), prior);
        presentation.configure([]);
        assert_eq!(
            rows.row_by_id("view", &original[0].id).unwrap(),
            original[0]
        );
    }

    #[test]
    fn workspace_capacity_refuses_new_publication_without_evicting_existing_results() {
        let (id, _) = fixture();
        let presentation = CommandPresentation::default();
        let make_rows = || {
            (0..4)
                .map(|offset| {
                    (
                        RecordId {
                            sequence: id.sequence + offset,
                            ..id
                        },
                        CommandResult {
                            fields: BTreeMap::from([(
                                "payload".into(),
                                json!("x".repeat(200 * 1024)),
                            )]),
                            diagnostic: None,
                        },
                    )
                })
                .collect::<CommandResults>()
        };
        presentation.configure((0..11).map(|index| (format!("view-{index}"), true)));
        for index in 0..10 {
            presentation
                .publish(&format!("view-{index}"), make_rows())
                .unwrap();
        }
        assert!(presentation.can_publish("view-10", &make_rows()).is_err());
        assert!(presentation.publish("view-10", make_rows()).is_err());
        assert_eq!(presentation.0.lock().unwrap().publications.len(), 10);
        presentation.configure((1..11).map(|index| (format!("view-{index}"), true)));
        presentation.publish("view-10", make_rows()).unwrap();
        assert_eq!(presentation.0.lock().unwrap().publications.len(), 10);
    }
}
