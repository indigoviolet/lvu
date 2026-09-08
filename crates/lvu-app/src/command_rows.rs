//! Read-only command results never participate in native membership predicates.
//!
//! Each command step of a view has its own publication, keyed by the step's
//! stage id and shown under the step's output name: `<name>.status`,
//! `<name>.<field>`, `<name>.diagnostic` in Details. The same rows are handed
//! to `lvu-view` as columns for later steps and filters; this module owns
//! only what the Details pane shows.
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

#[derive(Clone, Debug, PartialEq)]
pub struct CommandResult {
    pub fields: BTreeMap<String, Value>,
    pub diagnostic: Option<String>,
}

pub type CommandResults = BTreeMap<RecordId, CommandResult>;

/// A view's command step, as the presentation needs it: which stage, shown
/// under which name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandStepKey {
    pub stage: String,
    pub name: String,
}

struct Publication {
    rows: HashMap<RowId, CommandResult>,
    bytes: usize,
}

#[derive(Default)]
struct Shared {
    /// `(view, stage)` → published rows.
    publications: HashMap<(String, String), Publication>,
    /// The command steps of each view, in chain order.
    configured: HashMap<String, Vec<CommandStepKey>>,
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

    fn check_capacity(
        shared: &Shared,
        view: &str,
        stage: &str,
        bytes: usize,
    ) -> Result<(), String> {
        let other_bytes: usize = shared
            .publications
            .iter()
            .filter(|((id, step), _)| !(id == view && step == stage))
            .map(|(_, p)| p.bytes)
            .sum();
        if other_bytes.saturating_add(bytes) > MAX_ALL_PUBLICATION_BYTES {
            return Err("command results exceed the 8 MiB workspace presentation limit".into());
        }
        Ok(())
    }

    /// The controller serializes publication/restore while a durable save is pending.
    pub fn can_publish(
        &self,
        view: &str,
        stage: &str,
        rows: &CommandResults,
    ) -> Result<(), String> {
        let bytes = Self::publication_bytes(rows)?;
        Self::check_capacity(
            &self.0.lock().expect("command presentation poisoned"),
            view,
            stage,
            bytes,
        )
    }

    /// Admission is atomic: failure leaves the entire previous publication intact.
    pub fn publish(&self, view: &str, stage: &str, rows: CommandResults) -> Result<(), String> {
        let bytes = Self::publication_bytes(&rows)?;
        let mut shared = self.0.lock().expect("command presentation poisoned");
        Self::check_capacity(&shared, view, stage, bytes)?;
        shared.publications.insert(
            (view.into(), stage.into()),
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

    /// Drops a step's publication; the step is `Pending` again.
    pub fn clear(&self, view: &str, stage: &str) {
        let mut shared = self.0.lock().expect("command presentation poisoned");
        if shared
            .publications
            .remove(&(view.into(), stage.into()))
            .is_some()
        {
            shared.revision = shared.revision.wrapping_add(1);
        }
    }

    /// The command steps of every open view, in chain order. A step that
    /// left a chain takes its publication with it.
    pub fn configure(&self, views: impl IntoIterator<Item = (String, Vec<CommandStepKey>)>) {
        let next: HashMap<_, _> = views.into_iter().collect();
        let mut shared = self.0.lock().expect("command presentation poisoned");
        if shared.configured != next {
            shared.publications.retain(|(view, stage), _| {
                next.get(view)
                    .is_some_and(|steps| steps.iter().any(|step| step.stage == *stage))
            });
            shared.configured = next;
            shared.revision = shared.revision.wrapping_add(1);
        }
    }

    fn decorate(&self, view: &str, row: &mut DisplayRow) {
        let shared = self.0.lock().expect("command presentation poisoned");
        let Some(steps) = shared.configured.get(view) else {
            return;
        };
        for step in steps {
            let name = display_text(&step.name);
            let result = shared
                .publications
                .get(&(view.to_owned(), step.stage.clone()))
                .and_then(|p| p.rows.get(&row.id));
            match result {
                Some(result) => {
                    row.details
                        .push((format!("{name}.status"), "Ready · last explicit run".into()));
                    for (key, value) in &result.fields {
                        row.details.push((
                            format!("{name}.{}", display_text(key)),
                            match value {
                                Value::String(text) => display_text(text),
                                other => other.to_string(),
                            },
                        ));
                    }
                    if let Some(diagnostic) = &result.diagnostic {
                        row.details
                            .push((format!("{name}.diagnostic"), display_text(diagnostic)));
                    }
                }
                None => {
                    row.details
                        .push((format!("{name}.status"), "Pending — run explicitly".into()));
                }
            }
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
    fn unfolded_page(&self, view: &str, request: ViewportRequest) -> RowPage {
        let mut page = self.native.unfolded_page(view, request);
        for row in &mut page.rows {
            self.presentation.decorate(view, row);
        }
        page
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
    fn time_bounds(&self, view: &str, basis: lvu::TimeBasis) -> Option<lvu::TimeBounds> {
        self.native.time_bounds(view, basis)
    }
    fn view_order(&self, view: &str) -> Option<lvu::provider::ViewOrder> {
        self.native.view_order(view)
    }
    fn find_gap(
        &self,
        view: &str,
        from: Option<&RowId>,
        direction: lvu::GapDirection,
        threshold_nanos: i64,
        basis: lvu::TimeBasis,
    ) -> Option<lvu::GapHit> {
        self.native
            .find_gap(view, from, direction, threshold_nanos, basis)
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
        let step = || CommandStepKey {
            stage: "command-1".into(),
            name: "command".into(),
        };
        presentation.configure([("view".into(), vec![step()])]);
        let rows = CommandRows {
            native,
            presentation: presentation.clone(),
        };
        let before = rows.revision("view");
        presentation
            .publish(
                "view",
                "command-1",
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
        assert!(
            presentation
                .can_publish("view", "command-1", &too_big)
                .is_err()
        );
        assert!(presentation.publish("view", "command-1", too_big).is_err());
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
        let step = || {
            vec![CommandStepKey {
                stage: "s".into(),
                name: "command".into(),
            }]
        };
        presentation.configure((0..11).map(|index| (format!("view-{index}"), step())));
        for index in 0..10 {
            presentation
                .publish(&format!("view-{index}"), "s", make_rows())
                .unwrap();
        }
        assert!(
            presentation
                .can_publish("view-10", "s", &make_rows())
                .is_err()
        );
        assert!(presentation.publish("view-10", "s", make_rows()).is_err());
        assert_eq!(presentation.0.lock().unwrap().publications.len(), 10);
        presentation.configure((1..11).map(|index| (format!("view-{index}"), step())));
        presentation.publish("view-10", "s", make_rows()).unwrap();
        assert_eq!(presentation.0.lock().unwrap().publications.len(), 10);
    }
}
