use lvu_core::{
    Acquisition, CommandDefinition, CommandProgram, RecipeId, RestartPolicy, SourceDefinition,
    SourceId, ViewId,
};
use lvu_memory::*;
use rusqlite::Connection;
use std::{
    collections::BTreeMap,
    fs,
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
    assert_eq!(version, 2);
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
            &[("level", "str"), ("code", "i64")],
        ))
        .unwrap();
    store
        .upsert_source(&metadata(
            b.source.id,
            "beta",
            "other",
            7,
            &[("level", "bool")],
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
            .any(|v| v.contains("matching field types"))
    );
    assert!(result[0].score > result[1].score);
    assert!(matches!(
        store.candidates(target, None, None, &fields, 0),
        Err(MemoryError::InvalidLimit)
    ));
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
