use lvu_core::{
    Acquisition, CommandDefinition, CommandProgram, RecipeId, RecordId, RestartPolicy,
    SourceDefinition, SourceId, ViewId,
};
use lvu_memory::*;
use rusqlite::Connection;
use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    sync::{Arc, Barrier},
    thread,
    time::Duration,
};
use tempfile::TempDir;
use uuid::Uuid;

fn source(id: SourceId, name: &str) -> SourceDefinition {
    SourceDefinition {
        schema_version: 1,
        id,
        name: name.into(),
        acquisition: Acquisition::Command {
            command: CommandDefinition {
                program: CommandProgram::Shell {
                    text: "printf '%s\\n' ok".into(),
                },
                cwd: None,
                environment: BTreeMap::new(),
                restart: RestartPolicy::Never,
            },
        },
        identity_hints: BTreeMap::new(),
        retention: None,
    }
}
fn recipe(
    recipe_id: RecipeId,
    revision_id: Uuid,
    source_id: SourceId,
    expression: &str,
) -> RecipeFile {
    RecipeFile {
        schema_version: 1,
        recipe_id,
        revision_id,
        name: "Errors".into(),
        description: "portable".into(),
        source: source(source_id, "service"),
        view: NamedViewDefinition {
            schema_version: 1,
            id: ViewId::new(),
            name: "Error view".into(),
            source_ids: vec![source_id],
            stages: vec![StageDefinition::Polars {
                id: Uuid::new_v4(),
                expression: expression.into(),
                output: "level_normalized".into(),
            }],
            search: "timeout literal".into(),
            advanced_filter: Some(ExpressionDefinition {
                expression: "pl.col('level') == 'error'".into(),
            }),
            pinned_columns: vec!["level".into()],
            color_rules: vec![ColorRule {
                expression: "pl.col('level') == 'error'".into(),
                style: "red bold".into(),
            }],
            time_policy: TimePolicy::Recent { seconds: 300 },
            time_basis: TimeBasis::Event,
            grouping: Some(r"^(\s+|Caused by:)".into()),
        },
    }
}
fn metadata(
    id: SourceId,
    project: &str,
    command: &str,
    last_seen: i64,
    fields: &[(&str, &str)],
) -> SourceMetadata {
    SourceMetadata {
        definition: source(id, "source"),
        project: Some(project.into()),
        command: Some(command.into()),
        fields: fields
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        last_seen,
        missing: false,
    }
}

#[test]
fn toml_exactly_round_trips_quoted_multiline_expressions_and_rejects_protected_output() {
    let temp = TempDir::new().unwrap();
    let sid = SourceId::new();
    let rid = RecipeId::new();
    let expression = "pl.when(pl.col(\"message\").str.contains(\"quoted \\\"value\\\"\"))\n  .then(pl.lit(\"two\\nlines\"))\n  .otherwise(pl.col(\"message\"))";
    let value = recipe(rid, Uuid::new_v4(), sid, expression);
    let saved = save_recipe(temp.path(), &value, None).unwrap();
    let (loaded, hash) = read_recipe(&saved.path).unwrap();
    assert_eq!(loaded, value);
    assert_eq!(loaded.view.stages[0], value.view.stages[0]);
    assert_eq!(hash, saved.content_hash);
    let mut invalid = value;
    if let StageDefinition::Polars { output, .. } = &mut invalid.view.stages[0] {
        *output = "_lvu_sequence".into();
    }
    assert!(matches!(invalid.validate(), Err(RecipeError::Invalid(_))));
}

#[test]
fn named_recipe_listing_is_bounded_and_duplicate_names_are_explicit() {
    let temp = TempDir::new().unwrap();
    let mut store = WorkspaceStore::open(temp.path()).unwrap();
    let source = SourceId::new();
    let first = recipe(RecipeId::new(), Uuid::new_v4(), source, "pl.lit(1)");
    store.save_new_recipe(&first).unwrap();
    let mut duplicate = recipe(RecipeId::new(), Uuid::new_v4(), source, "pl.lit(2)");
    duplicate.name = first.name.clone();
    assert!(
        store
            .save_new_recipe(&duplicate)
            .unwrap_err()
            .to_string()
            .contains("already exists")
    );
    let listed = store.list_recipes(128).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].0, first);
    assert!(store.list_recipes(129).is_err());

    drop(store);
    let barrier = Arc::new(Barrier::new(2));
    let stores = [
        WorkspaceStore::open(temp.path()).unwrap(),
        WorkspaceStore::open(temp.path()).unwrap(),
    ];
    let mut workers = Vec::new();
    for (mut store, expression) in stores.into_iter().zip(["pl.lit(3)", "pl.lit(4)"]) {
        let barrier = Arc::clone(&barrier);
        let mut candidate = recipe(RecipeId::new(), Uuid::new_v4(), source, expression);
        candidate.name = "Concurrent".into();
        workers.push(thread::spawn(move || {
            barrier.wait();
            store.save_new_recipe(&candidate)
        }));
    }
    let results: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
}

fn private_view(id: ViewId, source_id: SourceId, name: &str, sequence: u64) -> WorkingView {
    WorkingView {
        id,
        source_id,
        name: name.into(),
        role: ViewRole::Derived,
        applied_revision_id: None,
        applied_search: name.into(),
        search_draft: Some(format!("{name} draft")),
        applied_advanced_filter: None,
        advanced_filter_draft: None,
        navigation: NavigationState {
            selected: Some(RecordId {
                source_id,
                sequence,
            }),
            anchor: None,
            follow: false,
        },
        presentation: PresentationState::default(),
        version: 0,
    }
}

#[test]
fn private_catalogues_serialize_same_name_and_reconcile_the_winner() {
    let root = TempDir::new().unwrap();
    let recipes = root.path().join("canonical/recipes");
    let workspaces = [root.path().join("slot-a"), root.path().join("slot-b")];
    let source_id = SourceId::new();
    let view_ids = [ViewId::new(), ViewId::new()];
    for (index, workspace) in workspaces.iter().enumerate() {
        let store = WorkspaceStore::open_with_recipes(workspace, &recipes).unwrap();
        store
            .upsert_source(&metadata(source_id, "private", "cmd", index as i64, &[]))
            .unwrap();
        store
            .create_view(&private_view(
                view_ids[index],
                source_id,
                if index == 0 { "a" } else { "b" },
                index as u64,
            ))
            .unwrap();
    }

    let barrier = Arc::new(Barrier::new(2));
    let stores = workspaces
        .iter()
        .map(|workspace| WorkspaceStore::open_with_recipes(workspace, &recipes).unwrap())
        .collect::<Vec<_>>();
    let mut threads = Vec::new();
    for (index, mut store) in stores.into_iter().enumerate() {
        let barrier = Arc::clone(&barrier);
        threads.push(thread::spawn(move || {
            let candidate = recipe(
                RecipeId::new(),
                Uuid::new_v4(),
                source_id,
                if index == 0 {
                    "pl.lit('a')"
                } else {
                    "pl.lit('b')"
                },
            );
            barrier.wait();
            store.save_new_recipe(&candidate)
        }));
    }
    let results: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);

    for (index, workspace) in workspaces.iter().enumerate() {
        let store = WorkspaceStore::open_with_recipes(workspace, &recipes).unwrap();
        assert_eq!(store.list_recipes(10).unwrap().len(), 1);
        assert!(store.get_view(view_ids[index]).unwrap().is_some());
        assert!(store.get_view(view_ids[1 - index]).unwrap().is_none());
    }
}

#[test]
fn private_catalogues_serialize_same_revision_and_loser_reconciles() {
    let root = TempDir::new().unwrap();
    let recipes = root.path().join("canonical/recipes");
    let workspaces = [root.path().join("slot-a"), root.path().join("slot-b")];
    let source_id = SourceId::new();
    let recipe_id = RecipeId::new();
    let base = recipe(recipe_id, Uuid::new_v4(), source_id, "pl.lit(0)");
    WorkspaceStore::open_with_recipes(&workspaces[0], &recipes)
        .unwrap()
        .save_new_recipe(&base)
        .unwrap();
    WorkspaceStore::open_with_recipes(&workspaces[1], &recipes).unwrap();

    let barrier = Arc::new(Barrier::new(2));
    let stores = workspaces
        .iter()
        .map(|workspace| WorkspaceStore::open_with_recipes(workspace, &recipes).unwrap())
        .collect::<Vec<_>>();
    let mut threads = Vec::new();
    for (index, mut store) in stores.into_iter().enumerate() {
        let barrier = Arc::clone(&barrier);
        let mut view = base.view.clone();
        view.search = format!("winner-{index}");
        threads.push(thread::spawn(move || {
            barrier.wait();
            store.update_recipe_revision(recipe_id, base.revision_id, &view)
        }));
    }
    let results: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    let winner = results
        .iter()
        .find_map(|result| result.as_ref().ok())
        .unwrap();

    for workspace in workspaces {
        let store = WorkspaceStore::open_with_recipes(workspace, &recipes).unwrap();
        let listed = store.list_recipes(10).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].0.revision_id, winner.revision_id);
        assert_eq!(store.recipe_history(recipe_id, None, 10).unwrap().len(), 2);
    }
}

#[test]
fn default_search_is_literal_case_insensitive_and_empty_is_unconstrained() {
    assert!(literal_search_matches("Request ERROR [a+b]", "error [A+B]"));
    assert!(literal_search_matches("anything", ""));
    assert!(!literal_search_matches("axb", "a.b"));
}

#[test]
fn unknown_or_malformed_toml_preserves_existing_bytes() {
    let temp = TempDir::new().unwrap();
    let mut value = recipe(
        RecipeId::new(),
        Uuid::new_v4(),
        SourceId::new(),
        "pl.col('x')",
    );
    value.schema_version = 99;
    let path = temp.path().join("future.toml");
    let bytes = toml::to_string(&value).unwrap().into_bytes();
    fs::write(&path, &bytes).unwrap();
    assert!(matches!(
        read_recipe(&path),
        Err(RecipeError::FutureVersion(99))
    ));
    assert_eq!(fs::read(&path).unwrap(), bytes);
    let malformed = b"schema_version = 1\nname = \"unterminated";
    fs::write(&path, malformed).unwrap();
    assert!(matches!(read_recipe(&path), Err(RecipeError::Toml(_))));
    assert_eq!(fs::read(&path).unwrap(), malformed);
}

#[test]
fn future_recipe_during_reconciliation_preserves_existing_database_state() {
    let temp = TempDir::new().unwrap();
    let sid = SourceId::new();
    {
        let store = WorkspaceStore::open(temp.path()).unwrap();
        store
            .upsert_source(&metadata(sid, "kept", "cmd", 7, &[]))
            .unwrap();
    }
    let mut future = recipe(
        RecipeId::new(),
        Uuid::new_v4(),
        SourceId::new(),
        "pl.col('x')",
    );
    future.schema_version = 2;
    let path = temp.path().join("recipes/future.toml");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let bytes = toml::to_string(&future).unwrap();
    fs::write(&path, &bytes).unwrap();
    assert!(matches!(
        WorkspaceStore::open(temp.path()),
        Err(MemoryError::Recipe(RecipeError::FutureVersion(2)))
    ));
    assert_eq!(fs::read_to_string(&path).unwrap(), bytes);
    fs::remove_file(path).unwrap();
    let reopened = WorkspaceStore::open(temp.path()).unwrap();
    assert_eq!(
        reopened.recent_sources(None, 10).unwrap()[0].definition.id,
        sid
    );
}

#[test]
fn save_never_overwrites_unknown_or_externally_changed_file() {
    let temp = TempDir::new().unwrap();
    let value = recipe(
        RecipeId::new(),
        Uuid::new_v4(),
        SourceId::new(),
        "pl.col('x')",
    );
    let path = recipe_path(temp.path(), value.recipe_id).unwrap();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"owner = 'someone else'").unwrap();
    assert!(matches!(
        save_recipe(temp.path(), &value, None),
        Err(RecipeError::Conflict | RecipeError::Toml(_))
    ));
    assert_eq!(fs::read(&path).unwrap(), b"owner = 'someone else'");
    assert!(
        recipe_path(temp.path(), value.recipe_id)
            .unwrap()
            .starts_with(temp.path().join("recipes"))
    );
}

#[test]
fn immutable_revision_conflict_never_poison_canonical_toml() {
    let temp = TempDir::new().unwrap();
    let mut store = WorkspaceStore::open(temp.path()).unwrap();
    let mut value = recipe(
        RecipeId::new(),
        Uuid::new_v4(),
        SourceId::new(),
        "pl.lit(1)",
    );
    let saved = store.save_recipe(&value, None).unwrap();
    let original = fs::read(&saved.path).unwrap();
    value.name = "changed without a new revision".into();
    assert!(matches!(
        store.save_recipe(&value, Some(&saved.content_hash)),
        Err(MemoryError::Conflict)
    ));
    assert_eq!(fs::read(&saved.path).unwrap(), original);
    drop(store);
    WorkspaceStore::open(temp.path()).unwrap();
}

#[test]
fn recipe_snapshot_never_rolls_back_current_source_metadata() {
    let temp = TempDir::new().unwrap();
    let sid = SourceId::new();
    let mut store = WorkspaceStore::open(temp.path()).unwrap();
    store
        .save_recipe(
            &recipe(RecipeId::new(), Uuid::new_v4(), sid, "pl.lit(1)"),
            None,
        )
        .unwrap();
    store
        .save_recipe(
            &recipe(RecipeId::new(), Uuid::new_v4(), sid, "pl.lit(2)"),
            None,
        )
        .unwrap();
    let mut latest = metadata(
        sid,
        "compose-project",
        "docker logs latest",
        100,
        &[("container", "str")],
    );
    latest.definition.name = "latest discovered container".into();
    store.upsert_source(&latest).unwrap();
    drop(store);
    let reopened = WorkspaceStore::open(temp.path()).unwrap();
    assert_eq!(reopened.recent_sources(None, 10).unwrap()[0], latest);
}

#[test]
fn stale_second_writer_cannot_reorder_file_and_database_revision() {
    let temp = TempDir::new().unwrap();
    let sid = SourceId::new();
    let rid = RecipeId::new();
    let mut first = WorkspaceStore::open(temp.path()).unwrap();
    let base = recipe(rid, Uuid::new_v4(), sid, "pl.lit(0)");
    let saved = first.save_recipe(&base, None).unwrap();
    let mut second = WorkspaceStore::open(temp.path()).unwrap();
    let mut a = recipe(rid, Uuid::new_v4(), sid, "pl.lit(1)");
    a.view.id = base.view.id;
    let mut b = recipe(rid, Uuid::new_v4(), sid, "pl.lit(2)");
    b.view.id = base.view.id;
    let current = first.save_recipe(&a, Some(&saved.content_hash)).unwrap();
    let bytes = fs::read(&current.path).unwrap();
    assert!(matches!(
        second.save_recipe(&b, Some(&saved.content_hash)),
        Err(MemoryError::Recipe(RecipeError::Conflict))
    ));
    assert_eq!(fs::read(&current.path).unwrap(), bytes);
    drop(first);
    drop(second);
    WorkspaceStore::open(temp.path()).unwrap();
}

#[test]
fn canonical_first_interruption_is_reconciled_on_open() {
    let temp = TempDir::new().unwrap();
    let sid = SourceId::new();
    let rid = RecipeId::new();
    let mut store = WorkspaceStore::open(temp.path()).unwrap();
    let first = recipe(rid, Uuid::new_v4(), sid, "pl.lit(0)");
    let saved = store.save_recipe(&first, None).unwrap();
    drop(store);
    let mut late = recipe(rid, Uuid::new_v4(), sid, "pl.lit(9)");
    late.view.id = first.view.id;
    let canonical = save_recipe(temp.path(), &late, Some(&saved.content_hash)).unwrap();
    let reopened = WorkspaceStore::open(temp.path()).unwrap();
    assert!(
        reopened
            .recipe_history(rid, None, 10)
            .unwrap()
            .iter()
            .any(|revision| revision.revision_id == late.revision_id)
    );
    assert_eq!(read_recipe(&canonical.path).unwrap().0, late);
}

#[test]
fn future_current_recipe_and_oversized_file_are_never_overwritten() {
    let temp = TempDir::new().unwrap();
    let valid = recipe(
        RecipeId::new(),
        Uuid::new_v4(),
        SourceId::new(),
        "pl.lit(1)",
    );
    let path = recipe_path(temp.path(), valid.recipe_id).unwrap();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut future = valid.clone();
    future.schema_version = 2;
    let bytes = toml::to_string(&future).unwrap().into_bytes();
    let hash = content_hash(&bytes);
    fs::write(&path, &bytes).unwrap();
    assert!(matches!(
        save_recipe(temp.path(), &valid, Some(&hash)),
        Err(RecipeError::FutureVersion(2))
    ));
    assert_eq!(fs::read(&path).unwrap(), bytes);
    let huge = temp.path().join("huge.toml");
    fs::write(&huge, vec![b'x'; MAX_DEFINITION_BYTES as usize + 1]).unwrap();
    assert!(matches!(read_recipe(&huge), Err(RecipeError::TooLarge)));
}

#[cfg(unix)]
#[test]
fn recipe_directory_symlink_cannot_escape_the_application_root() {
    use std::os::unix::fs::symlink;
    let root = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    symlink(outside.path(), root.path().join("recipes")).unwrap();
    let value = recipe(
        RecipeId::new(),
        Uuid::new_v4(),
        SourceId::new(),
        "pl.col('x')",
    );
    assert!(matches!(
        save_recipe(root.path(), &value, None),
        Err(RecipeError::UnsafePath)
    ));
    assert!(fs::read_dir(outside.path()).unwrap().next().is_none());
    assert!(matches!(
        WorkspaceStore::open(root.path()),
        Err(MemoryError::Recipe(RecipeError::UnsafePath))
    ));
}

#[test]
fn applied_revision_and_unfinished_draft_survive_reopen_independently() {
    let temp = TempDir::new().unwrap();
    let sid = SourceId::new();
    let revision = Uuid::new_v4();
    let id = ViewId::new();
    {
        let store = WorkspaceStore::open(temp.path()).unwrap();
        store
            .upsert_source(&metadata(sid, "p", "cmd", 4, &[]))
            .unwrap();
        store
            .create_view(&WorkingView {
                id,
                source_id: sid,
                name: "v".into(),
                role: ViewRole::Derived,
                applied_revision_id: Some(revision),
                applied_search: "error".into(),
                search_draft: Some("ERROR 42".into()),
                applied_advanced_filter: Some("pl.col('level') == 'error'".into()),
                advanced_filter_draft: Some(DraftState {
                    text: "pl.col(\"unfinished\")\n  == 1".into(),
                    diagnostics: vec!["missing )".into()],
                }),
                navigation: NavigationState {
                    selected: Some(lvu_core::RecordId {
                        source_id: sid,
                        sequence: 8,
                    }),
                    anchor: None,
                    follow: false,
                },
                presentation: PresentationState {
                    pinned_columns: vec!["service".into(), "request_id".into()],
                    color_field: Some("service".into()),
                    ..PresentationState::default()
                },
                version: 0,
            })
            .unwrap();
    }
    let store = WorkspaceStore::open(temp.path()).unwrap();
    let loaded = store.get_view(id).unwrap().unwrap();
    assert_eq!(loaded.applied_revision_id, Some(revision));
    assert_eq!(loaded.applied_search, "error");
    assert_eq!(loaded.search_draft.as_deref(), Some("ERROR 42"));
    assert_eq!(
        loaded.applied_advanced_filter.as_deref(),
        Some("pl.col('level') == 'error'")
    );
    assert_eq!(
        loaded.advanced_filter_draft.unwrap().text,
        "pl.col(\"unfinished\")\n  == 1"
    );
    assert_eq!(loaded.navigation.selected.unwrap().sequence, 8);
    assert_eq!(
        loaded.presentation.pinned_columns,
        ["service", "request_id"]
    );
    assert_eq!(loaded.presentation.color_field.as_deref(), Some("service"));
}

#[test]
fn version_one_workspace_migrates_to_default_presentation() {
    let temp = TempDir::new().unwrap();
    let db = temp.path().join("workspace.sqlite3");
    let connection = rusqlite::Connection::open(&db).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE working_views(view_id TEXT PRIMARY KEY,source_id TEXT NOT NULL,name TEXT NOT NULL,applied_revision_id TEXT,applied_search TEXT NOT NULL,search_draft TEXT,applied_advanced_filter TEXT,advanced_filter_draft_json BLOB,navigation_json BLOB NOT NULL,version INTEGER NOT NULL); PRAGMA user_version=1;",
        )
        .unwrap();
    let view_id = ViewId::new();
    let source_id = SourceId::new();
    let navigation = NavigationState {
        selected: Some(lvu_core::RecordId {
            source_id,
            sequence: 17,
        }),
        anchor: None,
        follow: false,
    };
    connection
        .execute(
            "INSERT INTO working_views VALUES(?1,?2,'saved',NULL,'error','unfinished',NULL,NULL,?3,0)",
            rusqlite::params![
                view_id.0.to_string(),
                source_id.0.to_string(),
                serde_json::to_vec(&navigation).unwrap()
            ],
        )
        .unwrap();
    drop(connection);
    let store = WorkspaceStore::open(temp.path()).unwrap();
    let mut loaded = store.get_view(view_id).unwrap().unwrap();
    assert_eq!(loaded.applied_search, "error");
    assert_eq!(loaded.search_draft.as_deref(), Some("unfinished"));
    assert_eq!(loaded.navigation.selected.as_ref().unwrap().sequence, 17);
    assert_eq!(loaded.presentation, PresentationState::default());
    loaded.presentation.pinned_columns = vec!["service".into()];
    loaded.presentation.color_field = Some("request_id".into());
    assert_eq!(store.update_view(&loaded, 0).unwrap(), 1);
    drop(store);

    let reopened = WorkspaceStore::open(temp.path()).unwrap();
    let loaded = reopened.get_view(view_id).unwrap().unwrap();
    assert_eq!(loaded.presentation.pinned_columns, ["service"]);
    assert_eq!(
        loaded.presentation.color_field.as_deref(),
        Some("request_id")
    );
    drop(reopened);
    let connection = rusqlite::Connection::open(db).unwrap();
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 6);
    let role: String = connection
        .query_row("SELECT role FROM working_views LIMIT 1", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        role, "derived",
        "a view that predates roles stays editable; nothing is promoted"
    );
    let value: String = connection
        .query_row(
            "SELECT dflt_value FROM pragma_table_info('working_views') WHERE name='presentation_json'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(value.contains("X'7B7D'"));
}

#[test]
fn views_on_one_source_keep_independent_navigation_and_conflict_versions() {
    let temp = TempDir::new().unwrap();
    let sid = SourceId::new();
    let store = WorkspaceStore::open(temp.path()).unwrap();
    store
        .upsert_source(&metadata(sid, "p", "cmd", 1, &[]))
        .unwrap();
    let make = |id, seq| WorkingView {
        id,
        source_id: sid,
        name: format!("v{seq}"),
        role: ViewRole::Derived,
        applied_revision_id: None,
        applied_search: String::new(),
        search_draft: None,
        applied_advanced_filter: None,
        advanced_filter_draft: None,
        navigation: NavigationState {
            selected: Some(lvu_core::RecordId {
                source_id: sid,
                sequence: seq,
            }),
            anchor: None,
            follow: seq % 2 == 0,
        },
        presentation: PresentationState::default(),
        version: 0,
    };
    let a = make(ViewId::new(), 1);
    let b = make(ViewId::new(), 9);
    store.create_view(&a).unwrap();
    store.create_view(&b).unwrap();
    let listed = store.working_views_for_source(sid, 8).unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(
        listed
            .iter()
            .map(|view| view.id)
            .collect::<std::collections::HashSet<_>>(),
        [a.id, b.id].into_iter().collect()
    );
    assert_ne!(
        store.get_view(a.id).unwrap().unwrap().navigation,
        store.get_view(b.id).unwrap().unwrap().navigation
    );
    let competing = WorkspaceStore::open(temp.path()).unwrap();
    let mut first = store.get_view(a.id).unwrap().unwrap();
    let mut stale = competing.get_view(a.id).unwrap().unwrap();
    first.name = "first".into();
    assert_eq!(store.update_view(&first, 0).unwrap(), 1);
    stale.name = "stale".into();
    assert!(matches!(
        competing.update_view(&stale, 0),
        Err(MemoryError::Conflict)
    ));
    let mut unusable = store.get_view(b.id).unwrap().unwrap();
    unusable.applied_search = "x".repeat(MAX_SEARCH_BYTES + 1);
    assert!(matches!(
        store.update_view(&unusable, 0),
        Err(MemoryError::InvalidData(_))
    ));
    let mut invalid_applied = store.get_view(b.id).unwrap().unwrap();
    invalid_applied.applied_advanced_filter = Some("  ".into());
    assert!(matches!(
        store.update_view(&invalid_applied, 0),
        Err(MemoryError::InvalidData(_))
    ));
    let mut invalid_draft = store.get_view(b.id).unwrap().unwrap();
    invalid_draft.advanced_filter_draft = Some(DraftState {
        text: "not valid Polars and intentionally allowed".into(),
        diagnostics: vec!["syntax".into()],
    });
    assert_eq!(store.update_view(&invalid_draft, 0).unwrap(), 1);
}

#[test]
fn external_import_installs_a_canonical_copy() {
    let external = TempDir::new().unwrap();
    let workspace = TempDir::new().unwrap();
    let value = recipe(
        RecipeId::new(),
        Uuid::new_v4(),
        SourceId::new(),
        "pl.lit(7)",
    );
    let external_path = external.path().join("shared.toml");
    fs::write(&external_path, toml::to_string_pretty(&value).unwrap()).unwrap();
    let mut store = WorkspaceStore::open(workspace.path()).unwrap();
    let installed = store.import_recipe(&external_path, None).unwrap();
    assert_eq!(
        installed.path,
        recipe_path(workspace.path(), value.recipe_id).unwrap()
    );
    fs::write(&external_path, "broken external copy").unwrap();
    drop(store);
    WorkspaceStore::open(workspace.path()).unwrap();
    assert_eq!(read_recipe(&installed.path).unwrap().0, value);
}

#[test]
fn immutable_revision_history_supports_undo_and_startup_reconciliation() {
    let temp = TempDir::new().unwrap();
    let sid = SourceId::new();
    let rid = RecipeId::new();
    let r1 = recipe(rid, Uuid::new_v4(), sid, "pl.col('a')");
    let mut store = WorkspaceStore::open(temp.path()).unwrap();
    let s1 = store.save_recipe(&r1, None).unwrap();
    let mut r2 = recipe(rid, Uuid::new_v4(), sid, "pl.col('b')");
    r2.view.id = r1.view.id;
    let s2 = store.save_recipe(&r2, Some(&s1.content_hash)).unwrap();
    let history = store.recipe_history(rid, None, 10).unwrap();
    assert_eq!(history.len(), 2);
    let restored = store
        .restore_recipe_revision(rid, r1.revision_id, &s2.content_hash)
        .unwrap();
    assert_eq!(read_recipe(&restored.path).unwrap().0, r1);
    drop(store);
    let reopened = WorkspaceStore::open(temp.path()).unwrap();
    let after = reopened.recipe_history(rid, None, 10).unwrap();
    assert_eq!(after.len(), 2);
    assert!(after.iter().any(|v| v.revision_id == r1.revision_id));
    let page = reopened
        .recipe_history(rid, Some(after[0].cursor), 1)
        .unwrap();
    assert_eq!(page.len(), 1);
    assert!(matches!(
        reopened.recipe_history(rid, None, 101),
        Err(MemoryError::InvalidLimit)
    ));
}

#[test]
fn recent_sources_retain_ids_missing_state_and_use_bounded_cursor_pages() {
    let temp = TempDir::new().unwrap();
    let store = WorkspaceStore::open(temp.path()).unwrap();
    let mut ids = Vec::new();
    for n in 0..4 {
        let id = SourceId::new();
        ids.push(id);
        let mut m = metadata(id, "p", "cmd", n, &[]);
        m.missing = n == 2;
        store.upsert_source(&m).unwrap();
        store
            .attach_fingerprint(id, "inode", &format!("i{n}"))
            .unwrap();
    }
    assert_eq!(
        store.source_by_fingerprint("inode", "i2").unwrap(),
        Some(ids[2])
    );
    let first = store.recent_sources(None, 2).unwrap();
    assert_eq!(first.len(), 2);
    let tail = first.last().unwrap();
    let second = store
        .recent_sources(Some((tail.last_seen, tail.definition.id)), 2)
        .unwrap();
    assert_eq!(second.len(), 2);
    assert!(first.iter().chain(&second).any(|v| v.missing));
    assert!(ids.contains(&first[0].definition.id));
    assert!(matches!(
        store.recent_sources(None, 101),
        Err(MemoryError::InvalidLimit)
    ));
}

#[test]
fn deterministic_candidates_explain_ranking_and_suggestion_outcomes() {
    let temp = TempDir::new().unwrap();
    let mut store = WorkspaceStore::open(temp.path()).unwrap();
    let target = SourceId::new();
    store
        .upsert_source(&metadata(
            target,
            "alpha",
            "serve",
            10,
            &[("level", "str"), ("code", "i64")],
        ))
        .unwrap();
    let a = recipe(
        RecipeId::new(),
        Uuid::new_v4(),
        SourceId::new(),
        "pl.col('a')",
    );
    let b = recipe(
        RecipeId::new(),
        Uuid::new_v4(),
        SourceId::new(),
        "pl.col('b')",
    );
    let sa = store.save_recipe(&a, None).unwrap();
    let _ = store.save_recipe(&b, None).unwrap();
    store
        .upsert_source(&metadata(
            a.source.id,
            "alpha",
            "serve",
            8,
            &[("level", "str"), ("code", "i64"), ("trace", "str")],
        ))
        .unwrap();
    store
        .upsert_source(&metadata(
            b.source.id,
            "beta",
            "other",
            7,
            &[("level", "bool"), ("trace", "str")],
        ))
        .unwrap();
    store.record_usage(target, a.recipe_id, 20).unwrap();
    store
        .record_suggestion(
            target,
            a.recipe_id,
            sa.revision_id,
            SuggestionOutcome::Accepted,
            21,
        )
        .unwrap();
    store
        .record_suggestion(
            target,
            b.recipe_id,
            b.revision_id,
            SuggestionOutcome::Rejected,
            22,
        )
        .unwrap();
    let fields = BTreeMap::from([
        ("level".into(), "str".into()),
        ("code".into(), "i64".into()),
    ]);
    let result = store
        .candidates(target, Some("alpha"), Some("serve"), &fields, 10)
        .unwrap();
    assert_eq!(result[0].recipe_id, a.recipe_id);
    assert!(result[0].evidence.contains(&"same project".into()));
    assert!(
        result[0]
            .evidence
            .iter()
            .any(|v| v.contains("matching authoritative field types"))
    );
    assert_eq!(result[0].missing_fields, vec!["trace"]);
    assert!(!result.iter().any(|value| value.recipe_id == b.recipe_id));
    assert!(matches!(
        store.candidates(target, None, None, &fields, 0),
        Err(MemoryError::InvalidLimit)
    ));
}

#[test]
fn empty_display_sample_does_not_erase_prior_field_evidence() {
    let temp = TempDir::new().unwrap();
    let store = WorkspaceStore::open(temp.path()).unwrap();
    let source = SourceId::new();
    store
        .upsert_source(&metadata(
            source,
            "project",
            "serve",
            1,
            &[("status", "display-text")],
        ))
        .unwrap();
    store
        .upsert_source(&metadata(source, "project", "serve", 2, &[]))
        .unwrap();
    let restored = store
        .recent_sources(None, 10)
        .unwrap()
        .into_iter()
        .find(|value| value.definition.id == source)
        .unwrap();
    assert_eq!(
        restored.fields.get("status").map(String::as_str),
        Some("display-text")
    );
}

#[test]
fn future_database_is_rejected_without_mutation_and_corruption_is_not_reset() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("workspace.sqlite3");
    let conn = Connection::open(&path).unwrap();
    conn.pragma_update(None, "user_version", 99).unwrap();
    drop(conn);
    let before = fs::read(&path).unwrap();
    assert!(matches!(
        WorkspaceStore::open(temp.path()),
        Err(MemoryError::FutureDatabase(99))
    ));
    assert_eq!(fs::read(&path).unwrap(), before);
    let bad = TempDir::new().unwrap();
    fs::write(bad.path().join("workspace.sqlite3"), b"not sqlite").unwrap();
    assert!(matches!(
        WorkspaceStore::open(bad.path()),
        Err(MemoryError::Database(_))
    ));
    assert_eq!(
        fs::read(bad.path().join("workspace.sqlite3")).unwrap(),
        b"not sqlite"
    );
}

#[test]
fn legacy_snapshot_preserves_private_state_and_recipe_history_idempotently() {
    let root = TempDir::new().unwrap();
    let legacy = root.path().join("legacy-workspace");
    let target = root.path().join("window-state/v1/slots/00/workspace");
    let migration = root.path().join("window-state/v1/migration");
    let target_recipes = root.path().join("window-state/v1/recipes");
    let source_id = SourceId::new();
    let view_id = ViewId::new();
    let recipe_id = RecipeId::new();
    let mut legacy_store = WorkspaceStore::open(&legacy).unwrap();
    legacy_store
        .upsert_source(&metadata(source_id, "legacy", "cmd", 7, &[]))
        .unwrap();
    let mut seeded_view = private_view(view_id, source_id, "accepted", 42);
    seeded_view.presentation.command_publication = Some("result-set-v3".into());
    legacy_store.create_view(&seeded_view).unwrap();
    let attempt_scope = attempt_scope(view_id);
    let attempt_record = attempt_id(source_id, 42);
    legacy_store
        .reserve_command_attempts(&attempt_scope, &[attempt_record], 8)
        .unwrap();
    let base = recipe(recipe_id, Uuid::new_v4(), source_id, "pl.lit(1)");
    legacy_store.save_new_recipe(&base).unwrap();
    let mut changed = base.view.clone();
    changed.search = "new revision".into();
    let current = legacy_store
        .update_recipe_revision(recipe_id, base.revision_id, &changed)
        .unwrap();
    drop(legacy_store);

    let seed = LegacyWorkspaceSeed {
        legacy_workspace_root: legacy.clone(),
        target_workspace_root: target.clone(),
        migration_root: migration.clone(),
        completion_marker: migration.join("slot-00-workspace-v1.complete"),
        version: 1,
    };
    WorkspaceStore::bootstrap_recipe_namespace(&RecipeSeed {
        legacy_recipes_root: legacy.join("recipes"),
        target_recipes_root: target_recipes.clone(),
        migration_root: migration.clone(),
        version: 1,
    })
    .unwrap();
    WorkspaceStore::open(&legacy)
        .unwrap()
        .upsert_source(&metadata(source_id, "late-legacy-write", "cmd", 99, &[]))
        .unwrap();
    assert_eq!(
        WorkspaceStore::import_legacy_snapshot(&seed).unwrap(),
        LegacyImportOutcome::Imported
    );
    let imported = WorkspaceStore::open_with_recipes(&target, &target_recipes).unwrap();
    assert_eq!(
        imported.get_view(view_id).unwrap().unwrap().applied_search,
        "accepted"
    );
    assert_eq!(
        imported
            .get_view(view_id)
            .unwrap()
            .unwrap()
            .presentation
            .command_publication
            .as_deref(),
        Some("result-set-v3")
    );
    assert_eq!(imported.command_attempt_count(&attempt_scope).unwrap(), 1);
    assert_eq!(
        imported
            .command_attempts(&attempt_scope, &[attempt_record])
            .unwrap()[0]
            .state,
        StoredCommandAttempt::Reserved
    );
    assert_eq!(
        imported.recipe_history(recipe_id, None, 10).unwrap().len(),
        2
    );
    assert_eq!(
        imported.list_recipes(10).unwrap()[0].0.revision_id,
        current.revision_id
    );
    assert_eq!(
        imported.recent_sources(None, 1).unwrap()[0]
            .project
            .as_deref(),
        Some("legacy")
    );
    imported
        .upsert_source(&metadata(source_id, "new-window", "cmd", 9, &[]))
        .unwrap();
    drop(imported);

    assert_eq!(
        WorkspaceStore::import_legacy_snapshot(&seed).unwrap(),
        LegacyImportOutcome::AlreadyImported
    );
    let reopened = WorkspaceStore::open_with_recipes(&target, &target_recipes).unwrap();
    assert_eq!(
        reopened.recent_sources(None, 1).unwrap()[0]
            .project
            .as_deref(),
        Some("new-window")
    );
    assert_eq!(
        WorkspaceStore::open(&legacy)
            .unwrap()
            .recent_sources(None, 1)
            .unwrap()[0]
            .project
            .as_deref(),
        Some("late-legacy-write")
    );
}

#[test]
fn empty_bootstrap_is_durable_and_late_legacy_database_is_never_adopted() {
    let root = TempDir::new().unwrap();
    let legacy = root.path().join("workspace");
    let target = root.path().join("window-state/v1/slots/00/workspace");
    let recipes = root.path().join("window-state/v1/recipes");
    let migration = root.path().join("window-state/v1/migration");
    WorkspaceStore::bootstrap_recipe_namespace(&RecipeSeed {
        legacy_recipes_root: legacy.join("recipes"),
        target_recipes_root: recipes.clone(),
        migration_root: migration.clone(),
        version: 1,
    })
    .unwrap();
    let seed = LegacyWorkspaceSeed {
        legacy_workspace_root: legacy.clone(),
        target_workspace_root: target.clone(),
        migration_root: migration.clone(),
        completion_marker: migration.join("slot-00-workspace-v1.complete"),
        version: 1,
    };
    assert_eq!(
        WorkspaceStore::import_legacy_snapshot(&seed).unwrap(),
        LegacyImportOutcome::NoLegacyDatabase
    );
    let source_id = SourceId::new();
    let target_store = WorkspaceStore::open_with_recipes(&target, &recipes).unwrap();
    target_store
        .upsert_source(&metadata(source_id, "new-window", "cmd", 3, &[]))
        .unwrap();
    drop(target_store);

    let late_id = SourceId::new();
    WorkspaceStore::open(&legacy)
        .unwrap()
        .upsert_source(&metadata(late_id, "late-legacy", "cmd", 9, &[]))
        .unwrap();
    assert_eq!(
        WorkspaceStore::import_legacy_snapshot(&seed).unwrap(),
        LegacyImportOutcome::AlreadyImported
    );
    let reopened = WorkspaceStore::open_with_recipes(&target, &recipes).unwrap();
    let sources = reopened.recent_sources(None, 10).unwrap();
    assert!(
        sources
            .iter()
            .any(|source| source.definition.id == source_id)
    );
    assert!(!sources.iter().any(|source| source.definition.id == late_id));
}

#[test]
fn slot_first_frozen_seed_mutation_is_rejected_before_workspace_import() {
    let root = TempDir::new().unwrap();
    let legacy = root.path().join("workspace");
    let target = root.path().join("window-state/v1/slots/00/workspace");
    let recipes = root.path().join("window-state/v1/recipes");
    let migration = root.path().join("window-state/v1/migration");
    let source_id = SourceId::new();
    WorkspaceStore::open(&legacy)
        .unwrap()
        .upsert_source(&metadata(source_id, "frozen", "cmd", 1, &[]))
        .unwrap();
    WorkspaceStore::bootstrap_recipe_namespace(&RecipeSeed {
        legacy_recipes_root: legacy.join("recipes"),
        target_recipes_root: recipes,
        migration_root: migration.clone(),
        version: 1,
    })
    .unwrap();
    let frozen = migration.join("workspace-v1.seed.sqlite3");
    Connection::open(&frozen)
        .unwrap()
        .execute(
            "UPDATE sources SET project='altered' WHERE source_id=?1",
            [source_id.0.to_string()],
        )
        .unwrap();
    let error = WorkspaceStore::import_legacy_snapshot(&LegacyWorkspaceSeed {
        legacy_workspace_root: legacy,
        target_workspace_root: target.clone(),
        migration_root: migration.clone(),
        completion_marker: migration.join("slot-00-workspace-v1.complete"),
        version: 1,
    })
    .unwrap_err();
    assert!(error.to_string().contains("immutable provenance"));
    assert!(!target.join("workspace.sqlite3").exists());
}

#[test]
fn recipe_bootstrap_preserves_bytes_and_isolates_unavailable_files() {
    let root = TempDir::new().unwrap();
    let legacy = root.path().join("workspace/recipes");
    let target = root.path().join("window-state/v1/recipes");
    let migration = root.path().join("window-state/v1/migration");
    fs::create_dir_all(&legacy).unwrap();
    fs::write(legacy.join(".write.lock"), b"").unwrap();
    let value = recipe(
        RecipeId::new(),
        Uuid::new_v4(),
        SourceId::new(),
        "pl.lit(1)",
    );
    let path = legacy.join(format!("{}.toml", value.recipe_id.0));
    let mut bytes = toml::to_string_pretty(&value).unwrap().into_bytes();
    bytes.extend_from_slice(b"\nadditive_unknown = \"preserve me\"\n");
    fs::write(&path, &bytes).unwrap();
    fs::write(legacy.join("future.toml"), b"schema_version = 2\n").unwrap();
    let seed = RecipeSeed {
        legacy_recipes_root: legacy,
        target_recipes_root: target.clone(),
        migration_root: migration.clone(),
        version: 1,
    };
    let report = WorkspaceStore::bootstrap_recipe_namespace(&seed).unwrap();
    assert_eq!(report.imported, 1);
    assert_eq!(report.unavailable.len(), 1);
    assert_eq!(
        fs::read(target.join(path.file_name().unwrap())).unwrap(),
        bytes
    );
    assert_eq!(
        WorkspaceStore::bootstrap_recipe_namespace(&seed).unwrap(),
        report
    );
}

#[test]
fn recipe_bootstrap_recovers_staging_and_serializes_slot_first_race() {
    let root = TempDir::new().unwrap();
    let legacy = root.path().join("workspace/recipes");
    let target = root.path().join("window-state/v1/recipes");
    let migration = root.path().join("window-state/v1/migration");
    fs::create_dir_all(&legacy).unwrap();
    fs::create_dir_all(&migration).unwrap();
    fs::write(legacy.join(".write.lock"), b"").unwrap();
    let value = recipe(
        RecipeId::new(),
        Uuid::new_v4(),
        SourceId::new(),
        "pl.lit(1)",
    );
    let name = format!("{}.toml", value.recipe_id.0);
    fs::write(legacy.join(&name), toml::to_string_pretty(&value).unwrap()).unwrap();
    let interrupted = migration.join("recipes-v1.pending");
    fs::create_dir(&interrupted).unwrap();
    fs::write(interrupted.join("partial.toml"), b"partial").unwrap();
    let seed = RecipeSeed {
        legacy_recipes_root: legacy,
        target_recipes_root: target.clone(),
        migration_root: migration,
        version: 1,
    };
    let barrier = Arc::new(Barrier::new(2));
    let mut workers = Vec::new();
    for _ in 0..2 {
        let barrier = Arc::clone(&barrier);
        let seed = seed.clone();
        workers.push(thread::spawn(move || {
            barrier.wait();
            WorkspaceStore::bootstrap_recipe_namespace(&seed)
        }));
    }
    for worker in workers {
        assert_eq!(worker.join().unwrap().unwrap().imported, 1);
    }
    assert!(target.is_dir());
    assert!(!target.join("partial.toml").exists());
    assert!(target.join(name).is_file());
}

#[test]
fn busy_writer_returns_a_bounded_error_without_losing_the_later_update() {
    let temp = TempDir::new().unwrap();
    let sid = SourceId::new();
    let initial = WorkspaceStore::open(temp.path()).unwrap();
    initial
        .upsert_source(&metadata(sid, "old", "cmd", 1, &[]))
        .unwrap();
    drop(initial);
    let mut lock_conn = Connection::open(temp.path().join("workspace.sqlite3")).unwrap();
    let lock = lock_conn.transaction().unwrap();
    lock.execute(
        "UPDATE sources SET project='locked' WHERE source_id=?1",
        [sid.0.to_string()],
    )
    .unwrap();
    let competing =
        WorkspaceStore::open_with_busy_timeout(temp.path(), Duration::from_millis(20)).unwrap();
    assert!(matches!(
        competing.upsert_source(&metadata(sid, "new", "cmd", 2, &[])),
        Err(MemoryError::Database(_))
    ));
    lock.rollback().unwrap();
    competing
        .upsert_source(&metadata(sid, "new", "cmd", 2, &[]))
        .unwrap();
    assert_eq!(
        competing.recent_sources(None, 1).unwrap()[0]
            .project
            .as_deref(),
        Some("new")
    );
}

#[test]
fn extraction_recipe_retains_named_regex_and_following_expression() {
    let root = TempDir::new().unwrap();
    let mut value = recipe(
        RecipeId::new(),
        Uuid::new_v4(),
        SourceId::new(),
        "pl.lit(1)",
    );
    value.view.stages = vec![
        StageDefinition::Extraction {
            id: "extract-request".into(),
            source: r"/request=(?P<request>\S+) status=(?P<status>\d+)/".into(),
        },
        StageDefinition::Extraction {
            id: "status-number".into(),
            source: "status_number = pl.col('status').cast(pl.Int64, strict=False)".into(),
        },
    ];
    let saved = save_recipe(root.path(), &value, None).unwrap();
    let (loaded, _) = read_recipe(&saved.path).unwrap();
    assert_eq!(loaded.view.stages, value.view.stages);
    value.view.stages.push(value.view.stages[0].clone());
    assert!(value.validate().is_err());
}

#[test]
fn legacy_enrichment_migration_and_explicit_empty_chain_are_distinct() {
    let legacy: PresentationState = serde_json::from_str(
        r#"{"applied_enrichment":"upper = pl.col('raw').str.to_uppercase()"}"#,
    )
    .unwrap();
    let chain = legacy.effective_enrichments();
    assert!(legacy.exact_field.is_none());
    assert_eq!(chain.len(), 1);
    assert_eq!(chain[0].id, "legacy-enrichment");
    let cleared = PresentationState {
        enrichment_chain: Some(Vec::new()),
        ..legacy
    };
    let reopened: PresentationState =
        serde_json::from_slice(&serde_json::to_vec(&cleared).unwrap()).unwrap();
    assert!(reopened.effective_enrichments().is_empty());
}

#[test]
fn exact_field_constraint_round_trips_in_presentation_state() {
    let exact = lvu_core::FieldCorrelation::new(
        "request.id",
        lvu_core::ExactScalar::string("01JZ café").unwrap(),
        [
            (
                "11111111-1111-4111-8111-111111111111".to_owned(),
                "request.id".to_owned(),
            ),
            (
                "22222222-2222-4222-8222-222222222222".to_owned(),
                "req".to_owned(),
            ),
        ]
        .into_iter()
        .collect(),
    )
    .unwrap();
    let state = PresentationState {
        exact_field: Some(exact.clone()),
        ..PresentationState::default()
    };
    let reopened: PresentationState =
        serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    assert_eq!(reopened.exact_field, Some(exact));
}

#[test]
fn ordered_enrichments_and_pending_edit_survive_database_reopen() {
    let root = TempDir::new().unwrap();
    let sid = SourceId::new();
    let id = ViewId::new();
    let chain = vec![
        StoredEnrichment {
            id: "one".into(),
            source: r"/id=(?P<id>\w+)/".into(),
        },
        StoredEnrichment {
            id: "two".into(),
            source: "upper_id = pl.col('id').str.to_uppercase()".into(),
        },
    ];
    {
        let store = WorkspaceStore::open(root.path()).unwrap();
        store
            .upsert_source(&metadata(sid, "project", "cmd", 1, &[]))
            .unwrap();
        let view = WorkingView {
            id,
            source_id: sid,
            name: "chain".into(),
            role: ViewRole::Derived,
            applied_revision_id: None,
            applied_search: String::new(),
            search_draft: None,
            applied_advanced_filter: None,
            advanced_filter_draft: None,
            navigation: NavigationState {
                selected: None,
                anchor: None,
                follow: true,
            },
            presentation: PresentationState {
                enrichment_chain: Some(chain.clone()),
                enrichment_editing: Some("two".into()),
                enrichment_selected: Some("two".into()),
                enrichment_draft: Some(DraftState {
                    text: "upper_id = pl.col(".into(),
                    diagnostics: vec!["unfinished".into()],
                }),
                ..Default::default()
            },
            version: 0,
        };
        store.create_view(&view).unwrap();
        let mut invalid = view.clone();
        invalid.id = ViewId::new();
        invalid
            .presentation
            .enrichment_chain
            .as_mut()
            .unwrap()
            .push(chain[0].clone());
        assert!(store.create_view(&invalid).is_err());
    }
    let store = WorkspaceStore::open(root.path()).unwrap();
    let loaded = store.get_view(id).unwrap().unwrap();
    assert_eq!(loaded.presentation.effective_enrichments(), chain);
    assert_eq!(
        loaded.presentation.enrichment_editing.as_deref(),
        Some("two")
    );
    assert_eq!(
        loaded.presentation.enrichment_draft.unwrap().text,
        "upper_id = pl.col("
    );
}

#[test]
fn extracted_time_basis_round_trips_in_portable_recipe() {
    let mut value = recipe(
        RecipeId::new(),
        Uuid::new_v4(),
        SourceId::new(),
        "pl.lit('x')",
    );
    value.view.time_basis = TimeBasis::Extracted;
    let text = toml::to_string(&value).unwrap();
    let restored: RecipeFile = toml::from_str(&text).unwrap();
    assert_eq!(restored.view.time_basis, TimeBasis::Extracted);
}

#[test]
fn exported_recipe_is_portable_revision_exact_and_never_overwrites() {
    let root = TempDir::new().unwrap();
    let mut store = WorkspaceStore::open(root.path().join("one")).unwrap();
    let original = recipe(
        RecipeId::new(),
        Uuid::new_v4(),
        SourceId::new(),
        "pl.col('raw').str.to_uppercase()",
    );
    let saved = store.save_new_recipe(&original).unwrap();
    let mut newer = original.clone();
    newer.revision_id = Uuid::new_v4();
    newer.view.search = "newer search".into();
    store
        .save_recipe(&newer, Some(&saved.content_hash))
        .unwrap();
    let output = root.path().join("portable recipe.toml");
    let exported = store
        .export_recipe_revision(original.recipe_id, original.revision_id, &output)
        .unwrap();
    let (read, hash) = read_recipe(&output).unwrap();
    assert_eq!(read, original);
    assert_eq!(hash, exported.content_hash);
    let mut other = WorkspaceStore::open(root.path().join("two")).unwrap();
    other.import_new_recipe(&output).unwrap();
    assert_eq!(other.list_recipes(128).unwrap()[0].0, original);
    let before = fs::read(&output).unwrap();
    assert!(
        store
            .export_recipe_revision(newer.recipe_id, newer.revision_id, &output)
            .is_err()
    );
    assert_eq!(fs::read(&output).unwrap(), before);
    assert!(
        store
            .export_recipe_revision(
                RecipeId::new(),
                original.revision_id,
                &root.path().join("missing.toml")
            )
            .is_err()
    );
    assert!(!root.path().join("missing.toml").exists());
    #[cfg(unix)]
    {
        let link = root.path().join("link.toml");
        std::os::unix::fs::symlink(&output, &link).unwrap();
        assert!(export_recipe(&link, &newer).is_err());
        assert_eq!(fs::read(&output).unwrap(), before);
    }
    assert!(!fs::read_dir(root.path()).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".lvu-export-")
    }));
}

#[test]
fn explicit_recipe_update_retains_history_and_rejects_stale_writers() {
    let temp = TempDir::new().unwrap();
    let mut first_store = WorkspaceStore::open(temp.path()).unwrap();
    let first = recipe(
        RecipeId::new(),
        Uuid::new_v4(),
        SourceId::new(),
        "pl.lit(1)",
    );
    first_store.save_new_recipe(&first).unwrap();
    let mut other_store = WorkspaceStore::open(temp.path()).unwrap();
    let mut updated = first.view.clone();
    updated.search = "new accepted text".into();
    updated.id = ViewId::new();
    updated.source_ids = vec![SourceId::new()];
    let saved = first_store
        .update_recipe_revision(first.recipe_id, first.revision_id, &updated)
        .unwrap();
    assert_ne!(saved.revision_id, first.revision_id);
    assert!(matches!(
        other_store.update_recipe_revision(first.recipe_id, first.revision_id, &first.view),
        Err(MemoryError::Conflict)
    ));
    drop(first_store);
    drop(other_store);
    let store = WorkspaceStore::open(temp.path()).unwrap();
    let history = store
        .recipe_revision_documents(first.recipe_id, 100)
        .unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[1], first);
    assert_eq!(history[0].revision_id, saved.revision_id);
    assert_eq!(history[0].source, first.source);
    assert_eq!(history[0].view.id, first.view.id);
    assert_eq!(history[0].view.name, first.view.name);
    assert_eq!(history[0].view.source_ids, first.view.source_ids);
    assert_eq!(history[0].view.search, "new accepted text");
    assert_eq!(store.list_recipes(128).unwrap()[0].0, history[0]);
    assert!(
        store
            .recipe_revision_documents(first.recipe_id, 101)
            .is_err()
    );
}

#[test]
fn ordered_sources_migrate_from_v2_and_preserve_cross_source_navigation_and_bookmarks() {
    let root = TempDir::new().unwrap();
    let owner = SourceId::new();
    let other = SourceId::new();
    let store = WorkspaceStore::open(root.path()).unwrap();
    store
        .upsert_source(&metadata(owner, "p", "a", 1, &[]))
        .unwrap();
    store
        .upsert_source(&metadata(other, "p", "b", 1, &[]))
        .unwrap();
    let id = ViewId::new();
    let mut view = WorkingView {
        id,
        source_id: owner,
        name: "merge".into(),
        role: ViewRole::Derived,
        applied_revision_id: None,
        applied_search: "keep".into(),
        search_draft: Some("unfinished".into()),
        applied_advanced_filter: None,
        advanced_filter_draft: None,
        navigation: NavigationState {
            selected: None,
            anchor: None,
            follow: true,
        },
        presentation: PresentationState::default(),
        version: 0,
    };
    store.create_view(&view).unwrap();
    drop(store);
    let db = root.path().join("workspace.sqlite3");
    let conn = Connection::open(&db).unwrap();
    conn.pragma_update(None, "user_version", 2).unwrap();
    drop(conn);
    let store = WorkspaceStore::open(root.path()).unwrap();
    assert_eq!(store.get_view(id).unwrap().unwrap(), view);
    view.presentation.source_ids = vec![other, owner];
    let record = lvu_core::RecordId {
        source_id: other,
        sequence: 7,
    };
    view.navigation.selected = Some(record);
    view.presentation.bookmarks.push(StoredBookmark {
        record,
        note: "other source note".into(),
    });
    view.version = store.update_view(&view, 0).unwrap();
    drop(store);
    let store = WorkspaceStore::open(root.path()).unwrap();
    assert_eq!(store.get_view(id).unwrap().unwrap(), view);
    view.presentation.source_ids = vec![owner];
    assert!(
        store.update_view(&view, 1).is_err(),
        "must not orphan a stored bookmark silently"
    );
    view.presentation.source_ids = vec![owner, owner, other];
    assert!(store.update_view(&view, 1).is_err());
    let conn = Connection::open(db).unwrap();
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        6
    );
}

fn attempt_scope(view_id: ViewId) -> CommandAttemptScope {
    CommandAttemptScope {
        view_id,
        stage_id: "normalize".into(),
        command_revision: "command-v1".into(),
        preceding_definition_revision: "chain-v7".into(),
    }
}

fn attempt_id(source_id: SourceId, sequence: u64) -> RecordId {
    RecordId {
        source_id,
        sequence,
    }
}

#[test]
fn reserved_attempts_survive_restart_and_preserve_full_u64_identity() {
    let root = TempDir::new().unwrap();
    let scope = attempt_scope(ViewId::new());
    let ids = [
        attempt_id(SourceId::new(), 0),
        attempt_id(SourceId::new(), u64::MAX),
    ];
    let reservation = {
        let mut store = WorkspaceStore::open(root.path()).unwrap();
        store.reserve_command_attempts(&scope, &ids, 8).unwrap()
    };
    let mut reopened = WorkspaceStore::open(root.path()).unwrap();
    assert_eq!(reopened.command_attempt_count(&scope).unwrap(), 2);
    assert_eq!(
        reopened.command_attempts(&scope, &ids).unwrap(),
        ids.iter()
            .map(|id| CommandAttemptRecord {
                record_id: *id,
                state: StoredCommandAttempt::Reserved
            })
            .collect::<Vec<_>>()
    );
    assert!(matches!(
        reopened.reserve_command_attempts(&scope, &[ids[0]], 8),
        Err(MemoryError::AlreadyAttempted(id)) if id == ids[0]
    ));
    assert_eq!(reservation.record_ids, ids);

    let mut changed_definition = scope.clone();
    changed_definition.command_revision = "command-v2".into();
    assert!(
        reopened
            .reserve_command_attempts(&changed_definition, &[ids[0]], 8)
            .is_ok()
    );
}

#[test]
fn completion_is_atomic_owned_and_cannot_replace_terminal_results() {
    let root = TempDir::new().unwrap();
    let scope = attempt_scope(ViewId::new());
    let source = SourceId::new();
    let ids = [attempt_id(source, 4), attempt_id(source, 5)];
    let mut store = WorkspaceStore::open(root.path()).unwrap();
    let reservation = store.reserve_command_attempts(&scope, &ids, 10).unwrap();
    let mut fields = BTreeMap::new();
    fields.insert(
        "severity".into(),
        serde_json::json!({"kind":"text","value":"élevé"}),
    );
    let outcomes = [
        (
            ids[1],
            CommandAttemptOutcome::Failed {
                diagnostic: "exit 7".into(),
            },
        ),
        (
            ids[0],
            CommandAttemptOutcome::Ready {
                fields: fields.clone(),
                diagnostic: Some("stderr note".into()),
            },
        ),
    ];
    let mut wrong = reservation.clone();
    wrong.token = Uuid::new_v4();
    assert!(matches!(
        store.complete_command_attempts(&wrong, &outcomes),
        Err(MemoryError::AttemptOwnership)
    ));
    assert!(
        store
            .command_attempts(&scope, &ids)
            .unwrap()
            .iter()
            .all(|item| item.state == StoredCommandAttempt::Reserved)
    );
    store
        .complete_command_attempts(&reservation, &outcomes)
        .unwrap();
    assert!(matches!(
        store.complete_command_attempts(&reservation, &outcomes),
        Err(MemoryError::AttemptOwnership)
    ));
    drop(store);

    let mut reopened = WorkspaceStore::open(root.path()).unwrap();
    assert_eq!(
        reopened
            .command_attempts(&scope, &[ids[1], ids[0]])
            .unwrap(),
        vec![
            CommandAttemptRecord {
                record_id: ids[1],
                state: StoredCommandAttempt::Failed {
                    diagnostic: "exit 7".into()
                }
            },
            CommandAttemptRecord {
                record_id: ids[0],
                state: StoredCommandAttempt::Ready {
                    fields,
                    diagnostic: Some("stderr note".into())
                }
            },
        ]
    );
    let new_id = attempt_id(source, 6);
    assert!(matches!(
        reopened.reserve_command_attempts(&scope, &[ids[0], new_id], 10),
        Err(MemoryError::AlreadyAttempted(id)) if id == ids[0]
    ));
    assert_eq!(
        reopened.command_attempts(&scope, &[new_id]).unwrap()[0].state,
        StoredCommandAttempt::NeverAttempted
    );
}

#[test]
fn concurrent_reservations_have_one_owner_and_capacity_never_evicts() {
    let root = TempDir::new().unwrap();
    let scope = attempt_scope(ViewId::new());
    let id = attempt_id(SourceId::new(), 42);
    let barrier = Arc::new(Barrier::new(2));
    let mut workers = Vec::new();
    for mut store in [
        WorkspaceStore::open(root.path()).unwrap(),
        WorkspaceStore::open(root.path()).unwrap(),
    ] {
        let barrier = Arc::clone(&barrier);
        let scope = scope.clone();
        workers.push(thread::spawn(move || {
            barrier.wait();
            store.reserve_command_attempts(&scope, &[id], 1)
        }));
    }
    let results = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
    let mut store = WorkspaceStore::open(root.path()).unwrap();
    assert!(matches!(
        store.reserve_command_attempts(&scope, &[attempt_id(id.source_id, 43)], 1),
        Err(MemoryError::AttemptCapacity { .. })
    ));
    assert_eq!(store.command_attempt_count(&scope).unwrap(), 1);
}

#[test]
fn malformed_or_oversized_batches_roll_back_without_losing_reservations() {
    let root = TempDir::new().unwrap();
    let scope = attempt_scope(ViewId::new());
    let source = SourceId::new();
    let ids = [attempt_id(source, 1), attempt_id(source, 2)];
    let mut store = WorkspaceStore::open(root.path()).unwrap();
    assert!(
        store
            .reserve_command_attempts(&scope, &[ids[0], ids[0]], 8)
            .is_err()
    );
    assert_eq!(store.command_attempt_count(&scope).unwrap(), 0);
    let too_many = (0..=MAX_COMMAND_ATTEMPT_BATCH)
        .map(|sequence| attempt_id(source, sequence as u64))
        .collect::<Vec<_>>();
    assert!(
        store
            .reserve_command_attempts(&scope, &too_many, too_many.len())
            .is_err()
    );
    let reservation = store.reserve_command_attempts(&scope, &ids, 8).unwrap();
    let incomplete = [(
        ids[0],
        CommandAttemptOutcome::Failed {
            diagnostic: "bad".into(),
        },
    )];
    assert!(
        store
            .complete_command_attempts(&reservation, &incomplete)
            .is_err()
    );
    let oversized = [
        (
            ids[0],
            CommandAttemptOutcome::Failed {
                diagnostic: "x".repeat(MAX_COMMAND_ATTEMPT_DIAGNOSTIC_BYTES + 1),
            },
        ),
        (
            ids[1],
            CommandAttemptOutcome::Failed {
                diagnostic: "bad".into(),
            },
        ),
    ];
    assert!(
        store
            .complete_command_attempts(&reservation, &oversized)
            .is_err()
    );
    assert!(
        store
            .command_attempts(&scope, &ids)
            .unwrap()
            .iter()
            .all(|item| item.state == StoredCommandAttempt::Reserved)
    );
}

#[test]
fn ownership_failure_mid_completion_rolls_back_prior_row_updates() {
    let root = TempDir::new().unwrap();
    let scope = attempt_scope(ViewId::new());
    let source = SourceId::new();
    let ids = [attempt_id(source, 10), attempt_id(source, 11)];
    let mut store = WorkspaceStore::open(root.path()).unwrap();
    let reservation = store.reserve_command_attempts(&scope, &ids, 8).unwrap();
    let connection = Connection::open(root.path().join("workspace.sqlite3")).unwrap();
    connection
        .execute(
            "UPDATE command_attempts SET state='failed',diagnostic='external owner loss' WHERE batch_token=?1 AND sequence=?2",
            rusqlite::params![reservation.token.to_string(), ids[1].sequence.to_string()],
        )
        .unwrap();
    drop(connection);
    let outcomes = [
        (
            ids[0],
            CommandAttemptOutcome::Failed {
                diagnostic: "first".into(),
            },
        ),
        (
            ids[1],
            CommandAttemptOutcome::Failed {
                diagnostic: "second".into(),
            },
        ),
    ];
    assert!(matches!(
        store.complete_command_attempts(&reservation, &outcomes),
        Err(MemoryError::AttemptOwnership)
    ));
    assert_eq!(
        store.command_attempts(&scope, &ids).unwrap(),
        vec![
            CommandAttemptRecord {
                record_id: ids[0],
                state: StoredCommandAttempt::Reserved,
            },
            CommandAttemptRecord {
                record_id: ids[1],
                state: StoredCommandAttempt::Failed {
                    diagnostic: "external owner loss".into(),
                },
            },
        ]
    );
}

#[test]
fn command_attempt_reads_preserve_order_and_enforce_cumulative_bytes() {
    let root = TempDir::new().unwrap();
    let scope = attempt_scope(ViewId::new());
    let source = SourceId::new();
    let mut store = WorkspaceStore::open(root.path()).unwrap();
    let mut ids = Vec::new();
    for sequence in 0..5 {
        let id = attempt_id(source, sequence);
        let reservation = store.reserve_command_attempts(&scope, &[id], 8).unwrap();
        let fields = BTreeMap::from([(
            "payload".into(),
            serde_json::Value::String("x".repeat(220 * 1024)),
        )]);
        store
            .complete_command_attempts(
                &reservation,
                &[(
                    id,
                    CommandAttemptOutcome::Ready {
                        fields,
                        diagnostic: None,
                    },
                )],
            )
            .unwrap();
        ids.push(id);
    }
    assert!(
        store
            .command_attempts(&scope, &ids)
            .unwrap_err()
            .to_string()
            .contains("requested command attempt results exceed")
    );
    let requested = [ids[3], ids[1]];
    let records = store.command_attempts(&scope, &requested).unwrap();
    assert_eq!(
        records
            .iter()
            .map(|item| item.record_id)
            .collect::<Vec<_>>(),
        requested
    );
}

#[test]
fn additive_v4_migration_preserves_sources_views_and_recipes() {
    let root = TempDir::new().unwrap();
    let source_id = SourceId::new();
    let view_id = ViewId::new();
    let recipe_id = RecipeId::new();
    {
        let mut store = WorkspaceStore::open(root.path()).unwrap();
        store
            .upsert_source(&metadata(source_id, "project", "command", 9, &[]))
            .unwrap();
        store
            .create_view(&WorkingView {
                id: view_id,
                source_id,
                name: "preserved".into(),
                role: ViewRole::Derived,
                applied_revision_id: None,
                applied_search: "error".into(),
                search_draft: None,
                applied_advanced_filter: None,
                advanced_filter_draft: None,
                navigation: NavigationState {
                    selected: None,
                    anchor: None,
                    follow: true,
                },
                presentation: PresentationState::default(),
                version: 0,
            })
            .unwrap();
        store
            .save_new_recipe(&recipe(
                recipe_id,
                Uuid::new_v4(),
                source_id,
                "pl.col('raw')",
            ))
            .unwrap();
    }
    let db = root.path().join("workspace.sqlite3");
    let connection = Connection::open(&db).unwrap();
    connection
        .execute_batch(
            "DROP TABLE command_attempts; DROP TABLE command_attempt_batches; PRAGMA user_version=3;",
        )
        .unwrap();
    drop(connection);

    let store = WorkspaceStore::open(root.path()).unwrap();
    assert_eq!(
        store.recent_sources(None, 10).unwrap()[0].definition.id,
        source_id
    );
    assert_eq!(
        store.get_view(view_id).unwrap().unwrap().applied_search,
        "error"
    );
    assert_eq!(store.list_recipes(10).unwrap()[0].0.recipe_id, recipe_id);
    let connection = Connection::open(db).unwrap();
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        6
    );
}

/// Fixture for the versioned slot layout: one shared canonical recipe root and
/// one migration namespace serving several private workspace slots.
struct SlotLayout {
    root: TempDir,
}

impl SlotLayout {
    fn new() -> Self {
        Self {
            root: TempDir::new().unwrap(),
        }
    }
    fn legacy(&self) -> PathBuf {
        self.root.path().join("workspace")
    }
    fn recipes(&self) -> PathBuf {
        self.root.path().join("window-state/v1/recipes")
    }
    fn migration(&self) -> PathBuf {
        self.root.path().join("window-state/v1/migration")
    }
    fn slot(&self, index: u32) -> PathBuf {
        self.root
            .path()
            .join(format!("window-state/v1/slots/{index:02}/workspace"))
    }
    fn recipe_seed(&self) -> RecipeSeed {
        RecipeSeed {
            legacy_recipes_root: self.legacy().join("recipes"),
            target_recipes_root: self.recipes(),
            migration_root: self.migration(),
            version: 1,
        }
    }
    fn workspace_seed(&self, index: u32) -> LegacyWorkspaceSeed {
        LegacyWorkspaceSeed {
            legacy_workspace_root: self.legacy(),
            target_workspace_root: self.slot(index),
            migration_root: self.migration(),
            completion_marker: self
                .migration()
                .join(format!("slot-{index:02}-workspace-v1.complete")),
            version: 1,
        }
    }
    fn open_slot(&self, index: u32) -> WorkspaceStore {
        WorkspaceStore::open_with_recipes(self.slot(index), self.recipes()).unwrap()
    }
}

fn history_ids(store: &WorkspaceStore, id: RecipeId) -> Vec<Uuid> {
    store
        .recipe_history(id, None, 100)
        .unwrap()
        .into_iter()
        .map(|revision| revision.revision_id)
        .collect()
}

#[test]
fn unmodified_frozen_seed_imports_and_only_the_bound_inode_is_accepted() {
    let layout = SlotLayout::new();
    let source_id = SourceId::new();
    let view_id = ViewId::new();
    let legacy = WorkspaceStore::open(layout.legacy()).unwrap();
    legacy
        .upsert_source(&metadata(source_id, "frozen-origin", "cmd", 5, &[]))
        .unwrap();
    legacy
        .create_view(&private_view(view_id, source_id, "accepted", 11))
        .unwrap();
    drop(legacy);
    WorkspaceStore::bootstrap_recipe_namespace(&layout.recipe_seed()).unwrap();

    let frozen = layout.migration().join("workspace-v1.seed.sqlite3");
    assert!(fs::metadata(&frozen).unwrap().len() > 0);
    assert_eq!(
        WorkspaceStore::import_legacy_snapshot(&layout.workspace_seed(0)).unwrap(),
        LegacyImportOutcome::Imported
    );
    let imported = layout.open_slot(0);
    assert_eq!(
        imported.get_view(view_id).unwrap().unwrap().applied_search,
        "accepted"
    );
    assert_eq!(
        imported.recent_sources(None, 1).unwrap()[0]
            .project
            .as_deref(),
        Some("frozen-origin")
    );
    drop(imported);
    assert_eq!(
        WorkspaceStore::import_legacy_snapshot(&layout.workspace_seed(0)).unwrap(),
        LegacyImportOutcome::AlreadyImported
    );
    assert_eq!(
        layout
            .open_slot(0)
            .get_view(view_id)
            .unwrap()
            .unwrap()
            .applied_search,
        "accepted"
    );

    // Replacing the seed path with a different file of identical length still
    // changes the bound inode, so a second slot refuses it.
    let substitute = layout.migration().join("substitute.sqlite3");
    let mut bytes = fs::read(&frozen).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    fs::write(&substitute, &bytes).unwrap();
    fs::rename(&substitute, &frozen).unwrap();
    let error = WorkspaceStore::import_legacy_snapshot(&layout.workspace_seed(1)).unwrap_err();
    assert!(error.to_string().contains("immutable provenance"));
    assert!(!layout.slot(1).join("workspace.sqlite3").exists());
}

#[test]
fn interrupted_seed_publication_recovers_or_fails_closed_in_both_orderings() {
    let frozen_name = "workspace-v1.seed.sqlite3";
    let provenance_name = "workspace-v1.seed.json";

    // Provenance published, seed lost, and the legacy source is still present:
    // an unpublished namespace re-freezes instead of wedging on create_new.
    let layout = SlotLayout::new();
    let source_id = SourceId::new();
    WorkspaceStore::open(layout.legacy())
        .unwrap()
        .upsert_source(&metadata(source_id, "recoverable", "cmd", 3, &[]))
        .unwrap();
    WorkspaceStore::bootstrap_recipe_namespace(&layout.recipe_seed()).unwrap();
    fs::remove_file(layout.migration().join(frozen_name)).unwrap();
    fs::remove_dir_all(layout.recipes()).unwrap();
    WorkspaceStore::bootstrap_recipe_namespace(&layout.recipe_seed()).unwrap();
    assert!(layout.migration().join(frozen_name).exists());
    assert_eq!(
        WorkspaceStore::import_legacy_snapshot(&layout.workspace_seed(0)).unwrap(),
        LegacyImportOutcome::Imported
    );
    assert_eq!(
        layout.open_slot(0).recent_sources(None, 1).unwrap()[0]
            .project
            .as_deref(),
        Some("recoverable")
    );

    // Seed published, provenance lost: the unvouched seed is discarded and
    // frozen again rather than imported on trust.
    let layout = SlotLayout::new();
    WorkspaceStore::open(layout.legacy())
        .unwrap()
        .upsert_source(&metadata(SourceId::new(), "reverse-order", "cmd", 3, &[]))
        .unwrap();
    WorkspaceStore::bootstrap_recipe_namespace(&layout.recipe_seed()).unwrap();
    fs::remove_file(layout.migration().join(provenance_name)).unwrap();
    fs::remove_dir_all(layout.recipes()).unwrap();
    WorkspaceStore::bootstrap_recipe_namespace(&layout.recipe_seed()).unwrap();
    assert_eq!(
        WorkspaceStore::import_legacy_snapshot(&layout.workspace_seed(0)).unwrap(),
        LegacyImportOutcome::Imported
    );
    assert_eq!(
        layout.open_slot(0).recent_sources(None, 1).unwrap()[0]
            .project
            .as_deref(),
        Some("reverse-order")
    );

    // Provenance published and both the seed and the legacy workspace are gone.
    // Nothing can reproduce the promised snapshot, so importing an empty slot
    // would silently discard it.
    let layout = SlotLayout::new();
    WorkspaceStore::open(layout.legacy())
        .unwrap()
        .upsert_source(&metadata(SourceId::new(), "unreproducible", "cmd", 3, &[]))
        .unwrap();
    WorkspaceStore::bootstrap_recipe_namespace(&layout.recipe_seed()).unwrap();
    fs::remove_file(layout.migration().join(frozen_name)).unwrap();
    fs::remove_dir_all(layout.legacy()).unwrap();
    let error = WorkspaceStore::import_legacy_snapshot(&layout.workspace_seed(0)).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("provenance exists without its seed")
    );
    assert!(!layout.slot(0).join("workspace.sqlite3").exists());
    fs::remove_dir_all(layout.recipes()).unwrap();
    let error = WorkspaceStore::bootstrap_recipe_namespace(&layout.recipe_seed()).unwrap_err();
    assert!(error.to_string().contains("no seed and no legacy source"));
}

#[test]
fn isolated_catalogues_share_every_published_revision_not_only_the_current_one() {
    let layout = SlotLayout::new();
    let source_id = SourceId::new();
    let recipe_id = RecipeId::new();
    let first = recipe(recipe_id, Uuid::new_v4(), source_id, "pl.lit(1)");
    let mut legacy = WorkspaceStore::open(layout.legacy()).unwrap();
    legacy.save_new_recipe(&first).unwrap();
    drop(legacy);
    WorkspaceStore::bootstrap_recipe_namespace(&layout.recipe_seed()).unwrap();

    // Slot 1 opens first and publishes two further revisions.
    WorkspaceStore::import_legacy_snapshot(&layout.workspace_seed(1)).unwrap();
    let mut slot_one = layout.open_slot(1);
    let mut second_view = first.view.clone();
    second_view.search = "second".into();
    let second = slot_one
        .update_recipe_revision(recipe_id, first.revision_id, &second_view)
        .unwrap();
    let mut third_view = first.view.clone();
    third_view.search = "third".into();
    let third = slot_one
        .update_recipe_revision(recipe_id, second.revision_id, &third_view)
        .unwrap();
    drop(slot_one);

    // Slot 0 bootstraps only afterwards and must not lose the middle revision.
    WorkspaceStore::import_legacy_snapshot(&layout.workspace_seed(0)).unwrap();
    let slot_zero = layout.open_slot(0);
    let slot_one = layout.open_slot(1);
    for catalogue in [&slot_zero, &slot_one] {
        let history = history_ids(catalogue, recipe_id);
        assert_eq!(history.len(), 3, "expected r1, r2 and r3");
        assert_eq!(
            history
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>(),
            [first.revision_id, second.revision_id, third.revision_id]
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>()
        );
        // The current pointer is imported last, so it heads the history.
        assert_eq!(history[0], third.revision_id);
        assert_eq!(
            catalogue.list_recipes(10).unwrap()[0].0.revision_id,
            third.revision_id
        );
    }
    // Every shared revision is exportable from the later catalogue, so the
    // history is real documents rather than bare identifiers.
    let exported = layout.root.path().join("second.toml");
    slot_zero
        .export_recipe_revision(recipe_id, second.revision_id, &exported)
        .unwrap();
    let (restored, _) = read_recipe(&exported).unwrap();
    assert_eq!(restored.view.search, "second");
}

#[test]
fn frozen_legacy_bootstrap_carries_history_published_before_the_freeze() {
    let layout = SlotLayout::new();
    let source_id = SourceId::new();
    let recipe_id = RecipeId::new();
    let first = recipe(recipe_id, Uuid::new_v4(), source_id, "pl.lit(1)");
    let mut legacy = WorkspaceStore::open(layout.legacy()).unwrap();
    legacy.save_new_recipe(&first).unwrap();
    let mut changed = first.view.clone();
    changed.search = "legacy second".into();
    let second = legacy
        .update_recipe_revision(recipe_id, first.revision_id, &changed)
        .unwrap();
    drop(legacy);

    let report = WorkspaceStore::bootstrap_recipe_namespace(&layout.recipe_seed()).unwrap();
    assert_eq!(report.imported, 1);
    assert!(report.unavailable.is_empty());
    let shared = layout
        .recipes()
        .join(".revisions")
        .join(recipe_id.0.to_string());
    for revision in [first.revision_id, second.revision_id] {
        assert!(
            shared.join(format!("{revision}.toml")).is_file(),
            "legacy revision {revision} is missing from the shared namespace"
        );
    }
    // A slot whose private workspace never saw the legacy database still
    // reconciles the complete legacy history from the shared namespace.
    let bare = layout.slot(7);
    fs::create_dir_all(&bare).unwrap();
    let catalogue = WorkspaceStore::open_with_recipes(&bare, layout.recipes()).unwrap();
    assert_eq!(
        history_ids(&catalogue, recipe_id)
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>(),
        [first.revision_id, second.revision_id]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
    );
}

#[test]
fn shared_history_beyond_its_bound_is_reported_rather_than_silently_dropped() {
    let layout = SlotLayout::new();
    let source_id = SourceId::new();
    let recipe_id = RecipeId::new();
    let current = recipe(recipe_id, Uuid::new_v4(), source_id, "pl.lit(1)");
    let mut legacy = WorkspaceStore::open(layout.legacy()).unwrap();
    legacy.save_new_recipe(&current).unwrap();
    drop(legacy);
    WorkspaceStore::bootstrap_recipe_namespace(&layout.recipe_seed()).unwrap();

    let shared = layout
        .recipes()
        .join(".revisions")
        .join(recipe_id.0.to_string());
    for index in 0..300 {
        let mut extra = recipe(recipe_id, Uuid::new_v4(), source_id, "pl.lit(1)");
        extra.view.search = format!("overflow {index}");
        fs::write(
            shared.join(format!("{}.toml", extra.revision_id)),
            toml::to_string_pretty(&extra).unwrap(),
        )
        .unwrap();
    }
    let bare = layout.slot(3);
    fs::create_dir_all(&bare).unwrap();
    let error = match WorkspaceStore::open_with_recipes(&bare, layout.recipes()) {
        Ok(_) => panic!("unbounded shared history was imported silently"),
        Err(error) => error,
    };
    assert!(
        matches!(error, MemoryError::ReconcileLimit),
        "unbounded history must refuse rather than import a silent subset: {error}"
    );
}

#[test]
fn one_admission_serialises_slot_bootstrap_across_slots() {
    let layout = SlotLayout::new();
    let source_id = SourceId::new();
    let recipe_id = RecipeId::new();
    let value = recipe(recipe_id, Uuid::new_v4(), source_id, "pl.lit(1)");
    let mut legacy = WorkspaceStore::open(layout.legacy()).unwrap();
    legacy
        .upsert_source(&metadata(source_id, "contended", "cmd", 4, &[]))
        .unwrap();
    legacy.save_new_recipe(&value).unwrap();
    drop(legacy);

    let recipe_seed = layout.recipe_seed();
    let seeds: Vec<_> = (0..3).map(|index| layout.workspace_seed(index)).collect();
    let barrier = Arc::new(Barrier::new(seeds.len()));
    let mut workers = Vec::new();
    for seed in seeds {
        let barrier = Arc::clone(&barrier);
        let recipe_seed = recipe_seed.clone();
        workers.push(thread::spawn(move || {
            barrier.wait();
            WorkspaceStore::bootstrap_recipe_namespace(&recipe_seed).unwrap();
            WorkspaceStore::import_legacy_snapshot(&seed).unwrap()
        }));
    }
    for worker in workers {
        assert_eq!(worker.join().unwrap(), LegacyImportOutcome::Imported);
    }
    for index in 0..3 {
        let catalogue = layout.open_slot(index);
        assert_eq!(
            catalogue.recent_sources(None, 1).unwrap()[0]
                .project
                .as_deref(),
            Some("contended")
        );
        assert_eq!(history_ids(&catalogue, recipe_id), vec![value.revision_id]);
    }
}

/// Migrating a workspace that predates roles must not reinterpret any view the
/// user already has. Every existing view stays exactly as it was, and the
/// canonical view is a new row beside them.
#[test]
fn canonical_view_is_created_beside_existing_views_and_never_adopts_them() {
    let root = TempDir::new().unwrap();
    let mut store = WorkspaceStore::open(root.path()).unwrap();
    let source_id = SourceId::new();
    let existing = ViewId::new();
    let mut working = private_view(existing, source_id, "Errors only", 7);
    working.applied_search = "level=error".into();
    store
        .save_source_and_view(&metadata(source_id, "p", "c", 1, &[]), &working, None)
        .unwrap();

    let preferred = ViewId::new();
    let canonical = store
        .ensure_canonical_view(source_id, preferred, "All events")
        .unwrap();
    assert_eq!(canonical.id, preferred);
    assert_eq!(canonical.role, ViewRole::Canonical);
    assert_eq!(canonical.name, "All events");
    assert!(canonical.applied_search.is_empty());
    assert!(canonical.applied_advanced_filter.is_none());

    let preserved = store.get_view(existing).unwrap().unwrap();
    assert_eq!(preserved.role, ViewRole::Derived);
    assert_eq!(preserved.name, "Errors only");
    assert_eq!(preserved.applied_search, "level=error");
    assert_eq!(
        store.working_views_for_source(source_id, 16).unwrap().len(),
        2
    );

    // Idempotent: a second call adopts the role metadata, not a fresh row.
    let again = store
        .ensure_canonical_view(source_id, ViewId::new(), "All events")
        .unwrap();
    assert_eq!(again.id, canonical.id);
    assert_eq!(
        store.working_views_for_source(source_id, 16).unwrap().len(),
        2
    );

    // Autosaving the canonical view must not be able to demote it, and
    // autosaving a derived view must not be able to promote it.
    let mut edited = store.get_view(canonical.id).unwrap().unwrap();
    edited.name = "All events".into();
    edited.navigation.follow = false;
    store.update_view(&edited, 0).unwrap();
    assert_eq!(
        store.get_view(canonical.id).unwrap().unwrap().role,
        ViewRole::Canonical
    );
    let mut derived = store.get_view(existing).unwrap().unwrap();
    derived.role = ViewRole::Canonical;
    store.update_view(&derived, derived.version).unwrap();
    assert_eq!(
        store.get_view(existing).unwrap().unwrap().role,
        ViewRole::Derived,
        "role is written once at creation; saves cannot change it"
    );
    assert_eq!(
        store
            .canonical_view_for_source(source_id)
            .unwrap()
            .unwrap()
            .id,
        canonical.id
    );
}

/// A source whose preferred canonical identity is already occupied by an
/// unrelated view still gets a canonical view, and the occupant is untouched.
#[test]
fn an_occupied_canonical_identity_yields_a_new_one_rather_than_a_takeover() {
    let root = TempDir::new().unwrap();
    let mut store = WorkspaceStore::open(root.path()).unwrap();
    let source_id = SourceId::new();
    let occupied = ViewId::new();
    let mut working = private_view(occupied, source_id, "Occupant", 1);
    working.applied_search = "keep me".into();
    store
        .save_source_and_view(&metadata(source_id, "p", "c", 1, &[]), &working, None)
        .unwrap();

    let canonical = store
        .ensure_canonical_view(source_id, occupied, "All events")
        .unwrap();
    assert_ne!(canonical.id, occupied);
    assert_eq!(canonical.role, ViewRole::Canonical);
    let survivor = store.get_view(occupied).unwrap().unwrap();
    assert_eq!(survivor.applied_search, "keep me");
    assert_eq!(survivor.role, ViewRole::Derived);
}

/// A workspace that kept bookmarks inside each view is migrated so the source
/// owns them. Nothing a user wrote is discarded: two notes for one record are
/// joined rather than one silently winning.
#[test]
fn per_view_bookmarks_move_to_their_source_and_keep_every_note() {
    let root = TempDir::new().unwrap();
    let database = root.path().join("workspace.sqlite3");
    let source_id = SourceId::new();
    let shared = RecordId {
        source_id,
        sequence: 7,
    };
    let (first, second) = (ViewId::new(), ViewId::new());
    {
        let mut store = WorkspaceStore::open(root.path()).unwrap();
        store
            .save_source_and_view(
                &metadata(source_id, "p", "c", 1, &[]),
                &private_view(first, source_id, "Errors", 1),
                None,
            )
            .unwrap();
        store
            .create_view(&private_view(second, source_id, "Warnings", 2))
            .unwrap();
    }

    // Rewrite the workspace into its pre-v6 shape: bookmarks inside each view.
    let presentation = |bookmarks: Vec<StoredBookmark>| {
        serde_json::to_vec(&PresentationState {
            bookmarks,
            ..PresentationState::default()
        })
        .unwrap()
    };
    {
        let connection = Connection::open(&database).unwrap();
        connection
            .execute("DELETE FROM source_bookmarks", [])
            .unwrap();
        connection
            .execute(
                "UPDATE working_views SET presentation_json=?2 WHERE view_id=?1",
                rusqlite::params![
                    first.0.to_string(),
                    presentation(vec![
                        StoredBookmark {
                            record: shared,
                            note: "seen from errors".into(),
                        },
                        StoredBookmark {
                            record: RecordId {
                                source_id,
                                sequence: 3,
                            },
                            note: "only in errors".into(),
                        },
                    ])
                ],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE working_views SET presentation_json=?2 WHERE view_id=?1",
                rusqlite::params![
                    second.0.to_string(),
                    presentation(vec![StoredBookmark {
                        record: shared,
                        note: "seen from warnings".into(),
                    }])
                ],
            )
            .unwrap();
        connection.pragma_update(None, "user_version", 5).unwrap();
    }

    let store = WorkspaceStore::open(root.path()).unwrap();
    let from_first = store
        .get_view(first)
        .unwrap()
        .unwrap()
        .presentation
        .bookmarks;
    let from_second = store
        .get_view(second)
        .unwrap()
        .unwrap()
        .presentation
        .bookmarks;
    assert_eq!(
        from_first, from_second,
        "every view of a source shows the same bookmarks"
    );
    assert_eq!(from_first.len(), 2, "{from_first:?}");
    let joined = from_first
        .iter()
        .find(|bookmark| bookmark.record == shared)
        .expect("the shared record");
    assert!(
        joined.note.contains("seen from errors") && joined.note.contains("seen from warnings"),
        "both notes are kept: {:?}",
        joined.note
    );
    assert!(
        from_first
            .iter()
            .any(|bookmark| bookmark.note == "only in errors"),
        "{from_first:?}"
    );

    // The view rows no longer hold a private copy that could resurrect a
    // deleted bookmark.
    let connection = Connection::open(&database).unwrap();
    let stored: Vec<u8> = connection
        .query_row(
            "SELECT presentation_json FROM working_views WHERE view_id=?1",
            [first.0.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    let stored: PresentationState = serde_json::from_slice(&stored).unwrap();
    assert!(stored.bookmarks.is_empty(), "{stored:?}");

    // Deleting through one view is durable, not undone by the other view.
    let mut view = store.get_view(first).unwrap().unwrap();
    view.presentation
        .bookmarks
        .retain(|bookmark| bookmark.record != shared);
    let version = view.version;
    store.update_view(&view, version).unwrap();
    assert_eq!(
        store
            .get_view(second)
            .unwrap()
            .unwrap()
            .presentation
            .bookmarks
            .len(),
        1
    );
}

/// The fold key column and its policy are additive JSON on the presentation
/// blob, so a view stored before they existed still opens and still means
/// "the derived pattern column" — which is what folding did then. No schema
/// version moves and no stored row is rewritten, which is the whole reason the
/// change needs no migration.
#[test]
fn a_view_stored_without_fold_key_fields_reads_as_the_derived_pattern_column() {
    let temp = TempDir::new().unwrap();
    let sid = SourceId::new();
    let id = ViewId::new();
    {
        let store = WorkspaceStore::open(temp.path()).unwrap();
        store
            .upsert_source(&metadata(sid, "p", "cmd", 4, &[]))
            .unwrap();
        store
            .create_view(&WorkingView {
                id,
                source_id: sid,
                name: "v".into(),
                role: ViewRole::Derived,
                applied_revision_id: None,
                applied_search: String::new(),
                search_draft: None,
                applied_advanced_filter: None,
                advanced_filter_draft: None,
                navigation: NavigationState {
                    selected: None,
                    anchor: None,
                    follow: true,
                },
                presentation: PresentationState {
                    fold_enabled: true,
                    fold_minimum_run: Some(4),
                    ..PresentationState::default()
                },
                version: 0,
            })
            .unwrap();
    }
    // Rewrite the stored blob as an older version would have written it: with
    // the three fold-key fields simply absent.
    {
        let connection = rusqlite::Connection::open(temp.path().join("workspace.sqlite3")).unwrap();
        let stored: Vec<u8> = connection
            .query_row(
                "SELECT presentation_json FROM working_views WHERE view_id=?1",
                rusqlite::params![id.0.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        let mut value: serde_json::Value = serde_json::from_slice(&stored).unwrap();
        let object = value.as_object_mut().unwrap();
        for field in ["fold_key_column", "fold_lookback", "fold_normalisation"] {
            assert!(object.remove(field).is_some(), "{field} must be persisted");
        }
        connection
            .execute(
                "UPDATE working_views SET presentation_json=?2 WHERE view_id=?1",
                rusqlite::params![id.0.to_string(), serde_json::to_vec(&value).unwrap()],
            )
            .unwrap();
    }

    let store = WorkspaceStore::open(temp.path()).unwrap();
    let mut loaded = store.get_view(id).unwrap().unwrap();
    assert!(loaded.presentation.fold_enabled);
    assert_eq!(loaded.presentation.fold_minimum_run, Some(4));
    assert_eq!(loaded.presentation.fold_key_column, None);
    assert_eq!(loaded.presentation.fold_lookback, 0);
    assert_eq!(loaded.presentation.fold_normalisation, "");

    // And a key column written now round-trips.
    loaded.presentation.fold_key_column = Some("service".into());
    loaded.presentation.fold_lookback = 8;
    loaded.presentation.fold_normalisation = "aggressive".into();
    let version = store.update_view(&loaded, loaded.version).unwrap();
    let reopened = WorkspaceStore::open(temp.path()).unwrap();
    let again = reopened.get_view(id).unwrap().unwrap();
    assert_eq!(
        again.presentation.fold_key_column.as_deref(),
        Some("service")
    );
    assert_eq!(again.presentation.fold_lookback, 8);
    assert_eq!(again.presentation.fold_normalisation, "aggressive");

    // The bounds the store enforces: an empty column name and a lookback wider
    // than the engine's cap are refused rather than silently clamped.
    let mut invalid = again.clone();
    invalid.presentation.fold_key_column = Some(String::new());
    assert!(matches!(
        reopened.update_view(&invalid, version),
        Err(MemoryError::InvalidData(_))
    ));
    let mut wide = again.clone();
    wide.presentation.fold_lookback = 4_097;
    assert!(matches!(
        reopened.update_view(&wide, version),
        Err(MemoryError::InvalidData(_))
    ));
}
