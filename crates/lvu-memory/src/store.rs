use crate::recipe::{RecipeLock, TimePolicy, save_recipe_locked};
use crate::{MAX_SEARCH_BYTES, RecipeError, RecipeFile, SavedRecipe, read_recipe, validate_source};
use lvu_core::{RecipeId, RecordId, SourceDefinition, SourceId, ViewId};
use rusqlite::{
    Connection, OptionalExtension, Transaction, TransactionBehavior, limits::Limit, params,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    time::Duration,
};
use uuid::Uuid;

const DB_SCHEMA_VERSION: i64 = 4;
const MAX_PAGE: u32 = 100;
const MAX_RECONCILE_FILES: usize = 1024;
const MAX_SQLITE_VALUE_BYTES: i32 = 1_200_000;
const MAX_CANDIDATE_SCAN: i64 = 128;
const MAX_EDITOR_BYTES: usize = 256 * 1024;
const MAX_DIAGNOSTICS: usize = 128;
pub const MAX_COMMAND_ATTEMPT_BATCH: usize = 1024;
pub const MAX_COMMAND_ATTEMPT_FIELDS: usize = 128;
pub const MAX_COMMAND_ATTEMPT_FIELD_BYTES: usize = 128;
pub const MAX_COMMAND_ATTEMPT_RESULT_BYTES: usize = 256 * 1024;
pub const MAX_COMMAND_ATTEMPT_DIAGNOSTIC_BYTES: usize = 16 * 1024;
pub const MAX_COMMAND_ATTEMPT_BATCH_BYTES: usize = 1024 * 1024;
const MAX_COMMAND_SCOPE_COMPONENT_BYTES: usize = 128;

fn source_family(definition: &SourceDefinition) -> &'static str {
    match definition.acquisition {
        lvu_core::Acquisition::File { .. } => "file",
        lvu_core::Acquisition::Command { .. } => "command",
        lvu_core::Acquisition::Http { .. } => "http",
        lvu_core::Acquisition::Stdin => "stdin",
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error(transparent)]
    Recipe(#[from] RecipeError),
    #[error("workspace database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("stored data is invalid: {0}")]
    InvalidData(String),
    #[error("unsupported database schema version {0}")]
    FutureDatabase(i64),
    #[error("concurrent update conflict")]
    Conflict,
    #[error("page limit must be between 1 and {MAX_PAGE}")]
    InvalidLimit,
    #[error("invalid command attempt batch: {0}")]
    InvalidAttemptBatch(String),
    #[error("record {0:?} was already attempted for this command definition")]
    AlreadyAttempted(RecordId),
    #[error(
        "command attempt capacity exceeded: {existing} stored + {requested} requested > {capacity}"
    )]
    AttemptCapacity {
        existing: usize,
        requested: usize,
        capacity: usize,
    },
    #[error("command attempt reservation is missing, completed, or owned by another batch")]
    AttemptOwnership,
    #[error("too many recipe files to reconcile (maximum {MAX_RECONCILE_FILES})")]
    ReconcileLimit,
    #[error("filesystem error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DraftState {
    pub text: String,
    pub diagnostics: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NavigationState {
    pub selected: Option<RecordId>,
    pub anchor: Option<RecordId>,
    pub follow: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StoredBookmark {
    pub record: RecordId,
    pub note: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PresentationState {
    /// Ordered working-view sources; empty is the legacy owning source.
    #[serde(default)]
    pub source_ids: Vec<SourceId>,
    #[serde(default)]
    pub bookmarks: Vec<StoredBookmark>,
    #[serde(default)]
    pub pinned_columns: Vec<String>,
    #[serde(default)]
    pub color_field: Option<String>,
    #[serde(default)]
    pub applied_enrichment: Option<String>,
    /// None means legacy single-stage state. Some([]) is explicitly cleared.
    #[serde(default)]
    pub enrichment_chain: Option<Vec<StoredEnrichment>>,
    #[serde(default)]
    pub enrichment_editing: Option<String>,
    #[serde(default)]
    pub enrichment_selected: Option<String>,
    #[serde(default)]
    pub enrichment_draft: Option<DraftState>,
    #[serde(default)]
    pub applied_grouping: Option<String>,
    #[serde(default)]
    pub grouping_draft: Option<DraftState>,
    #[serde(default)]
    pub capture_time: Option<TimePolicy>,
    #[serde(default)]
    pub time_basis: crate::TimeBasis,
    #[serde(default)]
    pub capture_time_start_draft: String,
    #[serde(default)]
    pub capture_time_end_draft: String,
    #[serde(default)]
    pub capture_time_error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StoredEnrichment {
    pub id: String,
    pub source: String,
}

impl PresentationState {
    pub fn effective_enrichments(&self) -> Vec<StoredEnrichment> {
        self.enrichment_chain.clone().unwrap_or_else(|| {
            self.applied_enrichment
                .as_ref()
                .filter(|source| !source.trim().is_empty())
                .map(|source| StoredEnrichment {
                    id: "legacy-enrichment".into(),
                    source: source.clone(),
                })
                .into_iter()
                .collect()
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct WorkingView {
    pub id: ViewId,
    pub source_id: SourceId,
    pub name: String,
    pub applied_revision_id: Option<Uuid>,
    pub applied_search: String,
    pub search_draft: Option<String>,
    pub applied_advanced_filter: Option<String>,
    pub advanced_filter_draft: Option<DraftState>,
    pub navigation: NavigationState,
    pub presentation: PresentationState,
    pub version: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceMetadata {
    pub definition: SourceDefinition,
    pub project: Option<String>,
    pub command: Option<String>,
    pub fields: BTreeMap<String, String>,
    pub last_seen: i64,
    pub missing: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecipeCandidate {
    pub recipe_id: RecipeId,
    pub revision_id: Uuid,
    pub name: String,
    pub score: i64,
    pub evidence: Vec<String>,
    pub missing_fields: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecipeRevisionSummary {
    pub cursor: i64,
    pub revision_id: Uuid,
    pub content_hash: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SuggestionOutcome {
    Accepted,
    Rejected,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandAttemptScope {
    pub view_id: ViewId,
    pub stage_id: String,
    pub command_revision: String,
    pub preceding_definition_revision: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandAttemptReservation {
    pub token: Uuid,
    pub scope: CommandAttemptScope,
    pub record_ids: Vec<RecordId>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum CommandAttemptOutcome {
    Ready {
        fields: BTreeMap<String, serde_json::Value>,
        diagnostic: Option<String>,
    },
    Failed {
        diagnostic: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StoredCommandAttempt {
    NeverAttempted,
    Reserved,
    Ready {
        fields: BTreeMap<String, serde_json::Value>,
        diagnostic: Option<String>,
    },
    Failed {
        diagnostic: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandAttemptRecord {
    pub record_id: RecordId,
    pub state: StoredCommandAttempt,
}

pub struct WorkspaceStore {
    conn: Connection,
    root: PathBuf,
}

impl WorkspaceStore {
    pub fn list_recipes(&self, limit: u32) -> Result<Vec<(RecipeFile, String)>, MemoryError> {
        if limit == 0 || limit > 128 {
            return Err(MemoryError::InvalidData(
                "recipe list limit must be 1..=128".into(),
            ));
        }
        let mut stmt = self.conn.prepare("SELECT rr.document, rr.content_hash FROM recipes r JOIN recipe_revisions rr ON rr.revision_id=r.current_revision_id ORDER BY r.name,r.recipe_id LIMIT ?1")?;
        let rows = stmt.query_map([limit], |row| {
            Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut values = Vec::new();
        for row in rows {
            let (bytes, hash) = row?;
            if bytes.len() as u64 > crate::MAX_DEFINITION_BYTES {
                return Err(RecipeError::TooLarge.into());
            }
            let text = std::str::from_utf8(&bytes)
                .map_err(|error| RecipeError::Toml(error.to_string()))?;
            let recipe: RecipeFile =
                toml::from_str(text).map_err(|error| RecipeError::Toml(error.to_string()))?;
            recipe.validate()?;
            values.push((recipe, hash));
        }
        Ok(values)
    }

    pub fn save_new_recipe(&mut self, recipe: &RecipeFile) -> Result<SavedRecipe, MemoryError> {
        recipe.validate()?;
        let guard = RecipeLock::acquire(&self.root, recipe.recipe_id)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM recipes WHERE name=?1 OR recipe_id=?2)",
            rusqlite::params![&recipe.name, recipe.recipe_id.0.to_string()],
            |row| row.get(0),
        )?;
        if exists {
            return Err(MemoryError::InvalidData(
                "a recipe with that name or identity already exists".into(),
            ));
        }
        preflight_revision(&tx, recipe)?;
        let saved = save_recipe_locked(&guard, recipe, None)?;
        import_tx(&tx, recipe, &saved.content_hash)?;
        tx.commit()?;
        Ok(saved)
    }

    /// Imports only a new identity/name. Existing recipes require the explicit
    /// optimistic revision API rather than silently moving their current pointer.
    pub fn import_new_recipe(&mut self, path: &Path) -> Result<SavedRecipe, MemoryError> {
        let (recipe, _) = read_recipe(path)?;
        self.save_new_recipe(&recipe)
    }
    pub fn open(root: impl AsRef<Path>) -> Result<Self, MemoryError> {
        Self::open_with_busy_timeout(root, Duration::from_secs(2))
    }

    pub fn open_with_busy_timeout(
        root: impl AsRef<Path>,
        busy_timeout: Duration,
    ) -> Result<Self, MemoryError> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root).map_err(|source| MemoryError::Io {
            path: root.clone(),
            source,
        })?;
        let db = root.join("workspace.sqlite3");
        let conn = Connection::open(&db)?;
        conn.set_limit(Limit::SQLITE_LIMIT_LENGTH, MAX_SQLITE_VALUE_BYTES)?;
        conn.busy_timeout(busy_timeout)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version > DB_SCHEMA_VERSION {
            return Err(MemoryError::FutureDatabase(version));
        }
        if version == 0 {
            migrate_v1(&conn)?;
        }
        if version < 2 {
            migrate_v2(&conn)?;
        }
        if version < 3 {
            // Ordered source membership changes the meaning of a working view.
            // Older applications must refuse this database rather than saving
            // a single-source interpretation over the persisted membership.
            conn.pragma_update(None, "user_version", 3)?;
        }
        if version < 4 {
            migrate_v4(&conn)?;
        }
        let store = Self { conn, root };
        store.reconcile_toml()?;
        Ok(store)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn save_recipe(
        &mut self,
        recipe: &RecipeFile,
        expected_hash: Option<&str>,
    ) -> Result<SavedRecipe, MemoryError> {
        recipe.validate()?;
        let guard = RecipeLock::acquire(&self.root, recipe.recipe_id)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        preflight_revision(&tx, recipe)?;
        // Publish canonical TOML before committing its SQLite pointer. A crash
        // in that narrow window is repaired by startup reconciliation.
        let saved = save_recipe_locked(&guard, recipe, expected_hash)?;
        import_tx(&tx, recipe, &saved.content_hash)?;
        tx.commit()?;
        Ok(saved)
    }

    /// Replace configuration only, retaining recipe identity and immutable history.
    /// The selected revision must still be current when the write lock is acquired.
    pub fn update_recipe_revision(
        &mut self,
        id: RecipeId,
        expected_revision: Uuid,
        view: &crate::NamedViewDefinition,
    ) -> Result<SavedRecipe, MemoryError> {
        let guard = RecipeLock::acquire(&self.root, id)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: Option<(String, String, Vec<u8>)> = tx.query_row(
            "SELECT rr.revision_id,rr.content_hash,rr.document FROM recipes r JOIN recipe_revisions rr ON rr.revision_id=r.current_revision_id WHERE r.recipe_id=?1",
            [id.0.to_string()], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).optional()?;
        let (revision, hash, bytes) = current.ok_or(MemoryError::Conflict)?;
        if revision != expected_revision.to_string() {
            return Err(MemoryError::Conflict);
        }
        let mut recipe = decode_recipe_revision(&bytes, id, expected_revision)?;
        let view_id = recipe.view.id;
        let view_name = recipe.view.name.clone();
        recipe.view = view.clone();
        recipe.view.id = view_id;
        recipe.view.name = view_name;
        recipe.view.source_ids = vec![recipe.source.id];
        recipe.revision_id = Uuid::new_v4();
        recipe.validate()?;
        preflight_revision(&tx, &recipe)?;
        let saved = save_recipe_locked(&guard, &recipe, Some(&hash))?;
        import_tx(&tx, &recipe, &saved.content_hash)?;
        tx.commit()?;
        Ok(saved)
    }

    pub fn recipe_revision_documents(
        &self,
        id: RecipeId,
        limit: u32,
    ) -> Result<Vec<RecipeFile>, MemoryError> {
        if limit == 0 || limit > 100 {
            return Err(MemoryError::InvalidData(
                "history limit must be 1..=100".into(),
            ));
        }
        self.recipe_history(id, None, limit)?
            .into_iter()
            .map(|revision| {
                let bytes: Vec<u8> = self.conn.query_row(
                    "SELECT document FROM recipe_revisions WHERE recipe_id=?1 AND revision_id=?2",
                    params![id.0.to_string(), revision.revision_id.to_string()],
                    |row| row.get(0),
                )?;
                decode_recipe_revision(&bytes, id, revision.revision_id)
            })
            .collect()
    }

    /// Installs an external recipe into the application-owned canonical path.
    pub fn import_recipe(
        &mut self,
        path: &Path,
        expected_hash: Option<&str>,
    ) -> Result<SavedRecipe, MemoryError> {
        let (recipe, _) = read_recipe(path)?;
        self.save_recipe(&recipe, expected_hash)
    }

    pub fn reconcile_toml(&self) -> Result<(), MemoryError> {
        let _guard = RecipeLock::acquire(&self.root, RecipeId(Uuid::nil()))?;
        let dir = self.root.join("recipes");
        match fs::symlink_metadata(&dir) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(RecipeError::UnsafePath.into());
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(source) => return Err(MemoryError::Io { path: dir, source }),
        }
        let entries = match fs::read_dir(&dir) {
            Ok(v) => v,
            Err(source) => return Err(MemoryError::Io { path: dir, source }),
        };
        let mut paths = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| MemoryError::Io {
                path: dir.clone(),
                source,
            })?;
            if entry
                .file_type()
                .map_err(|source| MemoryError::Io {
                    path: entry.path(),
                    source,
                })?
                .is_file()
                && entry.path().extension().is_some_and(|v| v == "toml")
            {
                paths.push(entry.path());
            }
            if paths.len() > MAX_RECONCILE_FILES {
                return Err(MemoryError::ReconcileLimit);
            }
        }
        paths.sort();
        // Validate every authoritative file before changing SQLite. A future or
        // malformed file therefore leaves all existing working state untouched.
        let parsed = paths
            .iter()
            .map(|path| read_recipe(path))
            .collect::<Result<Vec<_>, _>>()?;
        let tx = self.conn.unchecked_transaction()?;
        for (recipe, hash) in &parsed {
            import_tx(&tx, recipe, hash)?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn upsert_source(&self, source: &SourceMetadata) -> Result<(), MemoryError> {
        validate_source(&source.definition)?;
        self.conn.execute("INSERT INTO sources(source_id,definition_json,project,command,fields_json,last_seen,missing) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(source_id) DO UPDATE SET definition_json=excluded.definition_json,project=COALESCE(excluded.project,sources.project),command=COALESCE(excluded.command,sources.command),fields_json=CASE WHEN excluded.fields_json=x'7b7d' THEN sources.fields_json ELSE excluded.fields_json END,last_seen=excluded.last_seen,missing=excluded.missing", params![source.definition.id.0.to_string(), serde_json::to_vec(&source.definition).map_err(invalid)?, source.project, source.command, serde_json::to_vec(&source.fields).map_err(invalid)?, source.last_seen, source.missing])?;
        Ok(())
    }

    pub fn attach_fingerprint(
        &self,
        source_id: SourceId,
        kind: &str,
        value: &str,
    ) -> Result<(), MemoryError> {
        self.conn.execute(
            "INSERT OR IGNORE INTO source_fingerprints(source_id,kind,value) VALUES(?1,?2,?3)",
            params![source_id.0.to_string(), kind, value],
        )?;
        Ok(())
    }

    pub fn source_by_fingerprint(
        &self,
        kind: &str,
        value: &str,
    ) -> Result<Option<SourceId>, MemoryError> {
        self.conn
            .query_row(
                "SELECT source_id FROM source_fingerprints WHERE kind=?1 AND value=?2 ORDER BY source_id LIMIT 1",
                params![kind, value],
                |row| Ok(SourceId(parse_uuid(row.get(0)?)?)),
            )
            .optional()
            .map_err(MemoryError::from)
    }

    pub fn recent_sources(
        &self,
        before: Option<(i64, SourceId)>,
        limit: u32,
    ) -> Result<Vec<SourceMetadata>, MemoryError> {
        check_limit(limit)?;
        let (time, id) = before
            .map(|(t, id)| (t, id.0.to_string()))
            .unwrap_or((i64::MAX, "ffffffff-ffff-ffff-ffff-ffffffffffff".into()));
        let mut stmt = self.conn.prepare("SELECT definition_json,project,command,fields_json,last_seen,missing FROM sources WHERE (last_seen<?1 OR (last_seen=?1 AND source_id<?2)) ORDER BY last_seen DESC,source_id DESC LIMIT ?3")?;
        let rows = stmt.query_map(params![time, id, limit], source_row)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(MemoryError::from)
    }

    pub fn create_view(&self, view: &WorkingView) -> Result<(), MemoryError> {
        validate_working_view(view)?;
        self.conn.execute("INSERT INTO working_views(view_id,source_id,name,applied_revision_id,applied_search,search_draft,applied_advanced_filter,advanced_filter_draft_json,navigation_json,version,presentation_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)", params![view.id.0.to_string(), view.source_id.0.to_string(), view.name, view.applied_revision_id.map(|v|v.to_string()), view.applied_search, view.search_draft, view.applied_advanced_filter, json_opt(&view.advanced_filter_draft)?, serde_json::to_vec(&view.navigation).map_err(invalid)?, to_i64(view.version)?,serde_json::to_vec(&view.presentation).map_err(invalid)?])?;
        Ok(())
    }

    pub fn update_view(
        &self,
        view: &WorkingView,
        expected_version: u64,
    ) -> Result<u64, MemoryError> {
        validate_working_view(view)?;
        let next = expected_version
            .checked_add(1)
            .ok_or_else(|| MemoryError::InvalidData("view version overflow".into()))?;
        let changed = self.conn.execute("UPDATE working_views SET name=?2,applied_revision_id=?3,applied_search=?4,search_draft=?5,applied_advanced_filter=?6,advanced_filter_draft_json=?7,navigation_json=?8,version=?9,presentation_json=?11 WHERE view_id=?1 AND version=?10", params![view.id.0.to_string(), view.name, view.applied_revision_id.map(|v|v.to_string()), view.applied_search, view.search_draft, view.applied_advanced_filter, json_opt(&view.advanced_filter_draft)?, serde_json::to_vec(&view.navigation).map_err(invalid)?, to_i64(next)?, to_i64(expected_version)?,serde_json::to_vec(&view.presentation).map_err(invalid)?])?;
        if changed != 1 {
            return Err(MemoryError::Conflict);
        }
        Ok(next)
    }

    pub fn get_view(&self, id: ViewId) -> Result<Option<WorkingView>, MemoryError> {
        let value = self.conn.query_row("SELECT source_id,name,applied_revision_id,applied_search,search_draft,applied_advanced_filter,advanced_filter_draft_json,navigation_json,version,presentation_json FROM working_views WHERE view_id=?1", [id.0.to_string()], |r| {
            let version: i64 = r.get(8)?;
            Ok(WorkingView { id, source_id: SourceId(parse_uuid(r.get::<_,String>(0)?)?), name:r.get(1)?, applied_revision_id:r.get::<_,Option<String>>(2)?.map(parse_uuid).transpose()?, applied_search:r.get(3)?, search_draft:r.get(4)?, applied_advanced_filter:r.get(5)?, advanced_filter_draft:from_json_opt(r.get(6)?)?, navigation: serde_json::from_slice(&r.get::<_,Vec<u8>>(7)?).map_err(sql_invalid)?, version:u64::try_from(version).map_err(|e|rusqlite::Error::FromSqlConversionFailure(8,rusqlite::types::Type::Integer,Box::new(e)))?, presentation: serde_json::from_slice(&r.get::<_,Vec<u8>>(9)?).map_err(sql_invalid)? })
        }).optional().map_err(MemoryError::from)?;
        if let Some(view) = &value {
            validate_working_view(view)?;
        }
        Ok(value)
    }

    pub fn working_view_for_source(
        &self,
        source_id: SourceId,
    ) -> Result<Option<WorkingView>, MemoryError> {
        let id: Option<String> = self
            .conn
            .query_row(
                "SELECT view_id FROM working_views WHERE source_id=?1 ORDER BY view_id LIMIT 1",
                [source_id.0.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        id.map(|value| {
            let uuid = Uuid::parse_str(&value)
                .map_err(|error| MemoryError::InvalidData(error.to_string()))?;
            self.get_view(ViewId(uuid))?
                .ok_or_else(|| MemoryError::InvalidData("working view disappeared".into()))
        })
        .transpose()
    }

    pub fn working_views_for_source(
        &self,
        source_id: SourceId,
        limit: u32,
    ) -> Result<Vec<WorkingView>, MemoryError> {
        check_limit(limit)?;
        let mut statement = self.conn.prepare(
            "SELECT view_id FROM working_views WHERE source_id=?1 ORDER BY name,view_id LIMIT ?2",
        )?;
        let ids = statement
            .query_map(params![source_id.0.to_string(), limit], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        ids.into_iter()
            .map(|value| {
                let id = ViewId(parse_uuid(value)?);
                self.get_view(id)?
                    .ok_or_else(|| MemoryError::InvalidData("working view disappeared".into()))
            })
            .collect()
    }

    pub fn save_source_and_view(
        &mut self,
        source: &SourceMetadata,
        view: &WorkingView,
        expected_version: Option<u64>,
    ) -> Result<u64, MemoryError> {
        validate_source(&source.definition)?;
        validate_working_view(view)?;
        if source.definition.id != view.source_id {
            return Err(MemoryError::InvalidData(
                "view/source identity mismatch".into(),
            ));
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("INSERT INTO sources(source_id,definition_json,project,command,fields_json,last_seen,missing) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(source_id) DO UPDATE SET definition_json=excluded.definition_json,project=excluded.project,command=excluded.command,fields_json=excluded.fields_json,last_seen=excluded.last_seen,missing=excluded.missing", params![source.definition.id.0.to_string(), serde_json::to_vec(&source.definition).map_err(invalid)?, source.project, source.command, serde_json::to_vec(&source.fields).map_err(invalid)?, source.last_seen, source.missing])?;
        let version = match expected_version {
            None => {
                tx.execute("INSERT INTO working_views(view_id,source_id,name,applied_revision_id,applied_search,search_draft,applied_advanced_filter,advanced_filter_draft_json,navigation_json,version,presentation_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,0,?10)", params![view.id.0.to_string(),view.source_id.0.to_string(),view.name,view.applied_revision_id.map(|v|v.to_string()),view.applied_search,view.search_draft,view.applied_advanced_filter,json_opt(&view.advanced_filter_draft)?,serde_json::to_vec(&view.navigation).map_err(invalid)?,serde_json::to_vec(&view.presentation).map_err(invalid)?])?;
                0
            }
            Some(expected) => {
                let next = expected
                    .checked_add(1)
                    .ok_or_else(|| MemoryError::InvalidData("view version overflow".into()))?;
                let changed=tx.execute("UPDATE working_views SET name=?2,applied_revision_id=?3,applied_search=?4,search_draft=?5,applied_advanced_filter=?6,advanced_filter_draft_json=?7,navigation_json=?8,version=?9,presentation_json=?11 WHERE view_id=?1 AND version=?10",params![view.id.0.to_string(),view.name,view.applied_revision_id.map(|v|v.to_string()),view.applied_search,view.search_draft,view.applied_advanced_filter,json_opt(&view.advanced_filter_draft)?,serde_json::to_vec(&view.navigation).map_err(invalid)?,to_i64(next)?,to_i64(expected)?,serde_json::to_vec(&view.presentation).map_err(invalid)?])?;
                if changed != 1 {
                    return Err(MemoryError::Conflict);
                }
                next
            }
        };
        tx.commit()?;
        Ok(version)
    }

    pub fn recipe_history(
        &self,
        id: RecipeId,
        before_rowid: Option<i64>,
        limit: u32,
    ) -> Result<Vec<RecipeRevisionSummary>, MemoryError> {
        check_limit(limit)?;
        let before = before_rowid.unwrap_or(i64::MAX);
        let mut stmt = self.conn.prepare("SELECT rowid,revision_id,content_hash FROM recipe_revisions WHERE recipe_id=?1 AND rowid<?2 ORDER BY rowid DESC LIMIT ?3")?;
        let rows = stmt.query_map(params![id.0.to_string(), before, limit], |r| {
            Ok(RecipeRevisionSummary {
                cursor: r.get(0)?,
                revision_id: parse_uuid(r.get(1)?)?,
                content_hash: r.get(2)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(MemoryError::from)
    }

    /// Export the exact reviewed immutable revision, even if the current pointer changes.
    pub fn export_recipe_revision(
        &self,
        recipe: RecipeId,
        revision: Uuid,
        path: &Path,
    ) -> Result<SavedRecipe, MemoryError> {
        let document: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT document FROM recipe_revisions WHERE recipe_id=?1 AND revision_id=?2",
                params![recipe.0.to_string(), revision.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        let bytes = document
            .ok_or_else(|| MemoryError::InvalidData("revision does not belong to recipe".into()))?;
        if bytes.len() as u64 > crate::MAX_DEFINITION_BYTES {
            return Err(RecipeError::TooLarge.into());
        }
        let text =
            std::str::from_utf8(&bytes).map_err(|error| RecipeError::Toml(error.to_string()))?;
        let definition: RecipeFile =
            toml::from_str(text).map_err(|error| RecipeError::Toml(error.to_string()))?;
        if definition.recipe_id != recipe || definition.revision_id != revision {
            return Err(RecipeError::Conflict.into());
        }
        Ok(crate::export_recipe(path, &definition)?)
    }

    pub fn restore_recipe_revision(
        &mut self,
        recipe: RecipeId,
        revision: Uuid,
        expected_file_hash: &str,
    ) -> Result<SavedRecipe, MemoryError> {
        let document: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT document FROM recipe_revisions WHERE recipe_id=?1 AND revision_id=?2",
                params![recipe.0.to_string(), revision.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        let document = document
            .ok_or_else(|| MemoryError::InvalidData("revision does not belong to recipe".into()))?;
        let text =
            std::str::from_utf8(&document).map_err(|e| MemoryError::InvalidData(e.to_string()))?;
        let definition: RecipeFile =
            toml::from_str(text).map_err(|e| MemoryError::InvalidData(e.to_string()))?;
        definition.validate()?;
        self.save_recipe(&definition, Some(expected_file_hash))
    }

    pub fn record_usage(
        &self,
        source: SourceId,
        recipe: RecipeId,
        at: i64,
    ) -> Result<(), MemoryError> {
        self.conn.execute("INSERT INTO source_recipe_usage(source_id,recipe_id,use_count,last_used) VALUES(?1,?2,1,?3) ON CONFLICT(source_id,recipe_id) DO UPDATE SET use_count=use_count+1,last_used=excluded.last_used",params![source.0.to_string(),recipe.0.to_string(),at])?;
        Ok(())
    }
    pub fn record_suggestion(
        &self,
        source: SourceId,
        recipe: RecipeId,
        revision: Uuid,
        outcome: SuggestionOutcome,
        at: i64,
    ) -> Result<(), MemoryError> {
        self.conn.execute("INSERT INTO suggestion_outcomes(source_id,recipe_id,revision_id,outcome,recorded_at) VALUES(?1,?2,?3,?4,?5)",params![source.0.to_string(),recipe.0.to_string(),revision.to_string(),match outcome {SuggestionOutcome::Accepted=>"accepted",SuggestionOutcome::Rejected=>"rejected"},at])?;
        Ok(())
    }

    pub fn candidates(
        &self,
        source: SourceId,
        project: Option<&str>,
        command: Option<&str>,
        fields: &BTreeMap<String, String>,
        limit: u32,
    ) -> Result<Vec<RecipeCandidate>, MemoryError> {
        check_limit(limit)?;
        let target_definition = self
            .conn
            .query_row(
                "SELECT definition_json FROM sources WHERE source_id=?1",
                [source.0.to_string()],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()?
            .and_then(|bytes| serde_json::from_slice::<SourceDefinition>(&bytes).ok());
        let target_family = target_definition.as_ref().map(source_family);
        let mut stmt=self.conn.prepare("SELECT r.recipe_id,r.current_revision_id,r.name,s.project,s.command,s.fields_json,s.definition_json,COALESCE(u.use_count,0),COALESCE(u.last_used,0),COALESCE((SELECT SUM(CASE outcome WHEN 'accepted' THEN 1 ELSE -1 END) FROM suggestion_outcomes o WHERE o.source_id=?1 AND o.recipe_id=r.recipe_id),0) FROM recipes r JOIN sources s ON s.source_id=r.source_id LEFT JOIN source_recipe_usage u ON u.source_id=?1 AND u.recipe_id=r.recipe_id ORDER BY (s.project=?2) DESC,(s.command=?3) DESC,COALESCE(u.use_count,0) DESC,r.recipe_id LIMIT ?4")?;
        let mut candidates = Vec::new();
        let rows = stmt.query_map(
            params![source.0.to_string(), project, command, MAX_CANDIDATE_SCAN],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, Vec<u8>>(5)?,
                    r.get::<_, Vec<u8>>(6)?,
                    r.get::<_, i64>(7)?,
                    r.get::<_, i64>(8)?,
                    r.get::<_, i64>(9)?,
                ))
            },
        )?;
        for row in rows {
            let (rid, rev, name, p, c, fjson, definition_json, uses, last, outcome) = row?;
            let candidate_fields: BTreeMap<String, String> =
                serde_json::from_slice(&fjson).map_err(invalid)?;
            let mut evidence = Vec::new();
            let mut score = uses.min(20) + outcome * 20;
            if outcome > 0 {
                evidence.push(format!("net {outcome} prior acceptance signal"));
            } else if outcome < 0 {
                evidence.push(format!("net {} prior rejection signal", -outcome));
            }
            if project.is_some() && project == p.as_deref() {
                score += 100;
                evidence.push("same project".into());
            }
            if command.is_some() && command == c.as_deref() {
                score += 80;
                evidence.push("same command".into());
            }
            if let (Some(target), Ok(candidate)) = (
                target_family.as_ref(),
                serde_json::from_slice::<SourceDefinition>(&definition_json),
            ) && *target == source_family(&candidate)
            {
                score += 40;
                evidence.push(format!("same {target} source family"));
            }
            let exact = fields
                .iter()
                .filter(|(k, v)| candidate_fields.get(*k) == Some(*v))
                .count() as i64;
            let names = fields
                .keys()
                .filter(|k| candidate_fields.contains_key(*k))
                .count() as i64;
            score += exact * 10 + (names - exact) * 3;
            if exact > 0 && fields.values().all(|value| value == "display-text") {
                evidence.push(format!(
                    "{exact} field names observed in sampled visible rows"
                ));
            } else if exact > 0 {
                evidence.push(format!("{exact} matching authoritative field types"));
            } else if names > 0 {
                evidence.push(format!("{names} matching field names"));
            }
            let missing_fields = candidate_fields
                .keys()
                .filter(|field| !fields.contains_key(*field))
                .take(32)
                .cloned()
                .collect();
            if uses > 0 {
                evidence.push(format!("used {uses} times on this source"));
            }
            if last > 0 {
                evidence.push(format!("last used {last}"));
            }
            // A shared acquisition family is the minimum reviewable evidence;
            // unrelated acquisition kinds with no other signal are omitted.
            if score < 40 {
                continue;
            }
            candidates.push(RecipeCandidate {
                recipe_id: RecipeId(parse_uuid(rid)?),
                revision_id: parse_uuid(rev)?,
                name,
                score,
                evidence,
                missing_fields,
            });
        }
        candidates.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| a.recipe_id.cmp(&b.recipe_id))
        });
        candidates.truncate(limit as usize);
        Ok(candidates)
    }

    pub fn command_attempt_count(&self, scope: &CommandAttemptScope) -> Result<usize, MemoryError> {
        validate_attempt_scope(scope)?;
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM command_attempts WHERE view_id=?1 AND stage_id=?2 AND command_revision=?3 AND preceding_definition_revision=?4",
            params![scope.view_id.0.to_string(), scope.stage_id, scope.command_revision, scope.preceding_definition_revision],
            |row| row.get(0),
        )?;
        usize::try_from(count).map_err(|error| MemoryError::InvalidData(error.to_string()))
    }

    pub fn reserve_command_attempts(
        &mut self,
        scope: &CommandAttemptScope,
        record_ids: &[RecordId],
        capacity: usize,
    ) -> Result<CommandAttemptReservation, MemoryError> {
        validate_attempt_scope(scope)?;
        let ids = validate_attempt_ids(record_ids)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing: i64 = tx.query_row(
            "SELECT COUNT(*) FROM command_attempts WHERE view_id=?1 AND stage_id=?2 AND command_revision=?3 AND preceding_definition_revision=?4",
            params![scope.view_id.0.to_string(), scope.stage_id, scope.command_revision, scope.preceding_definition_revision],
            |row| row.get(0),
        )?;
        let existing = usize::try_from(existing)
            .map_err(|error| MemoryError::InvalidData(error.to_string()))?;
        for id in &ids {
            let attempted: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM command_attempts WHERE view_id=?1 AND stage_id=?2 AND command_revision=?3 AND preceding_definition_revision=?4 AND source_id=?5 AND sequence=?6)",
                params![scope.view_id.0.to_string(), scope.stage_id, scope.command_revision, scope.preceding_definition_revision, id.source_id.0.to_string(), id.sequence.to_string()],
                |row| row.get(0),
            )?;
            if attempted {
                return Err(MemoryError::AlreadyAttempted(*id));
            }
        }
        if existing.saturating_add(ids.len()) > capacity {
            return Err(MemoryError::AttemptCapacity {
                existing,
                requested: ids.len(),
                capacity,
            });
        }
        let token = Uuid::new_v4();
        tx.execute(
            "INSERT INTO command_attempt_batches(token,view_id,stage_id,command_revision,preceding_definition_revision,expected_count,completed) VALUES(?1,?2,?3,?4,?5,?6,0)",
            params![token.to_string(), scope.view_id.0.to_string(), scope.stage_id, scope.command_revision, scope.preceding_definition_revision, ids.len() as i64],
        )?;
        for id in &ids {
            tx.execute(
                "INSERT INTO command_attempts(view_id,stage_id,command_revision,preceding_definition_revision,source_id,sequence,batch_token,state) VALUES(?1,?2,?3,?4,?5,?6,?7,'reserved')",
                params![scope.view_id.0.to_string(), scope.stage_id, scope.command_revision, scope.preceding_definition_revision, id.source_id.0.to_string(), id.sequence.to_string(), token.to_string()],
            )?;
        }
        tx.commit()?;
        Ok(CommandAttemptReservation {
            token,
            scope: scope.clone(),
            record_ids: ids,
        })
    }

    pub fn complete_command_attempts(
        &mut self,
        reservation: &CommandAttemptReservation,
        outcomes: &[(RecordId, CommandAttemptOutcome)],
    ) -> Result<(), MemoryError> {
        validate_attempt_scope(&reservation.scope)?;
        let reserved = validate_attempt_ids(&reservation.record_ids)?;
        let outcome_ids =
            validate_attempt_ids(&outcomes.iter().map(|(id, _)| *id).collect::<Vec<_>>())?;
        if reserved.iter().copied().collect::<BTreeSet<_>>()
            != outcome_ids.iter().copied().collect::<BTreeSet<_>>()
        {
            return Err(MemoryError::InvalidAttemptBatch(
                "outcomes must exactly match the reserved record IDs".into(),
            ));
        }
        let encoded = outcomes
            .iter()
            .map(|(id, outcome)| encode_attempt_outcome(*id, outcome))
            .collect::<Result<Vec<_>, _>>()?;
        let total_bytes = encoded
            .iter()
            .try_fold(0usize, |total, item| total.checked_add(item.payload_bytes));
        if total_bytes.is_none_or(|bytes| bytes > MAX_COMMAND_ATTEMPT_BATCH_BYTES) {
            return Err(MemoryError::InvalidAttemptBatch(format!(
                "completion payload exceeds {MAX_COMMAND_ATTEMPT_BATCH_BYTES} bytes"
            )));
        }

        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let batch: Option<(String, String, String, String, i64, bool)> = tx
            .query_row(
                "SELECT view_id,stage_id,command_revision,preceding_definition_revision,expected_count,completed FROM command_attempt_batches WHERE token=?1",
                [reservation.token.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
            )
            .optional()?;
        let Some((view, stage, command, preceding, count, completed)) = batch else {
            return Err(MemoryError::AttemptOwnership);
        };
        if completed
            || view != reservation.scope.view_id.0.to_string()
            || stage != reservation.scope.stage_id
            || command != reservation.scope.command_revision
            || preceding != reservation.scope.preceding_definition_revision
            || usize::try_from(count).ok() != Some(encoded.len())
        {
            return Err(MemoryError::AttemptOwnership);
        }
        for item in encoded {
            let changed = tx.execute(
                "UPDATE command_attempts SET state=?8,fields_json=?9,diagnostic=?10 WHERE view_id=?1 AND stage_id=?2 AND command_revision=?3 AND preceding_definition_revision=?4 AND source_id=?5 AND sequence=?6 AND batch_token=?7 AND state='reserved'",
                params![view, stage, command, preceding, item.id.source_id.0.to_string(), item.id.sequence.to_string(), reservation.token.to_string(), item.state, item.fields, item.diagnostic],
            )?;
            if changed != 1 {
                return Err(MemoryError::AttemptOwnership);
            }
        }
        let changed = tx.execute(
            "UPDATE command_attempt_batches SET completed=1 WHERE token=?1 AND completed=0",
            [reservation.token.to_string()],
        )?;
        if changed != 1 {
            return Err(MemoryError::AttemptOwnership);
        }
        tx.commit()?;
        Ok(())
    }

    pub fn command_attempts(
        &self,
        scope: &CommandAttemptScope,
        record_ids: &[RecordId],
    ) -> Result<Vec<CommandAttemptRecord>, MemoryError> {
        validate_attempt_scope(scope)?;
        let ids = validate_attempt_ids(record_ids)?;
        let mut records = Vec::with_capacity(ids.len());
        let mut total_bytes = 0usize;
        for id in ids {
            let stored: Option<(String, Option<Vec<u8>>, Option<String>)> = self
                .conn
                .query_row(
                    "SELECT state,fields_json,diagnostic FROM command_attempts WHERE view_id=?1 AND stage_id=?2 AND command_revision=?3 AND preceding_definition_revision=?4 AND source_id=?5 AND sequence=?6",
                    params![scope.view_id.0.to_string(), scope.stage_id, scope.command_revision, scope.preceding_definition_revision, id.source_id.0.to_string(), id.sequence.to_string()],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;
            let stored_bytes = stored.as_ref().map_or(0, |(_, fields, diagnostic)| {
                fields.as_ref().map_or(0, Vec::len) + diagnostic.as_ref().map_or(0, String::len)
            });
            total_bytes = total_bytes.checked_add(stored_bytes).ok_or_else(|| {
                MemoryError::InvalidData("command attempt read size overflow".into())
            })?;
            if total_bytes > MAX_COMMAND_ATTEMPT_BATCH_BYTES {
                return Err(MemoryError::InvalidAttemptBatch(format!(
                    "requested command attempt results exceed {MAX_COMMAND_ATTEMPT_BATCH_BYTES} bytes"
                )));
            }
            records.push(CommandAttemptRecord {
                record_id: id,
                state: decode_attempt_state(stored)?,
            });
        }
        Ok(records)
    }
}

fn migrate_v1(conn: &Connection) -> Result<(), MemoryError> {
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch("CREATE TABLE sources(source_id TEXT PRIMARY KEY,definition_json BLOB NOT NULL,project TEXT,command TEXT,fields_json BLOB NOT NULL DEFAULT '{}',last_seen INTEGER NOT NULL DEFAULT 0,missing INTEGER NOT NULL DEFAULT 0); CREATE TABLE source_fingerprints(source_id TEXT NOT NULL,kind TEXT NOT NULL,value TEXT NOT NULL,PRIMARY KEY(source_id,kind,value),FOREIGN KEY(source_id) REFERENCES sources(source_id)); CREATE TABLE recipe_revisions(revision_id TEXT PRIMARY KEY,recipe_id TEXT NOT NULL,name TEXT NOT NULL,content_hash TEXT NOT NULL,document BLOB NOT NULL); CREATE TABLE recipes(recipe_id TEXT PRIMARY KEY,current_revision_id TEXT NOT NULL,name TEXT NOT NULL,source_id TEXT NOT NULL,FOREIGN KEY(current_revision_id) REFERENCES recipe_revisions(revision_id)); CREATE TABLE source_recipe_usage(source_id TEXT NOT NULL,recipe_id TEXT NOT NULL,use_count INTEGER NOT NULL,last_used INTEGER NOT NULL,PRIMARY KEY(source_id,recipe_id),FOREIGN KEY(source_id) REFERENCES sources(source_id),FOREIGN KEY(recipe_id) REFERENCES recipes(recipe_id)); CREATE TABLE suggestion_outcomes(id INTEGER PRIMARY KEY,source_id TEXT NOT NULL,recipe_id TEXT NOT NULL,revision_id TEXT NOT NULL,outcome TEXT NOT NULL CHECK(outcome IN('accepted','rejected')),recorded_at INTEGER NOT NULL); CREATE TABLE working_views(view_id TEXT PRIMARY KEY,source_id TEXT NOT NULL,name TEXT NOT NULL,applied_revision_id TEXT,applied_search TEXT NOT NULL,search_draft TEXT,applied_advanced_filter TEXT,advanced_filter_draft_json BLOB,navigation_json BLOB NOT NULL,version INTEGER NOT NULL,FOREIGN KEY(source_id) REFERENCES sources(source_id)); CREATE INDEX sources_recent_idx ON sources(last_seen DESC,source_id DESC); CREATE INDEX fingerprints_lookup_idx ON source_fingerprints(kind,value,source_id); CREATE INDEX suggestion_lookup_idx ON suggestion_outcomes(source_id,recipe_id,outcome); CREATE INDEX recipe_history_idx ON recipe_revisions(recipe_id); PRAGMA user_version=1;")?;
    tx.commit()?;
    Ok(())
}
fn migrate_v2(conn: &Connection) -> Result<(), MemoryError> {
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(
        "ALTER TABLE working_views ADD COLUMN presentation_json BLOB NOT NULL DEFAULT X'7B7D';\
         PRAGMA user_version=2;",
    )?;
    tx.commit()?;
    Ok(())
}
fn migrate_v4(conn: &Connection) -> Result<(), MemoryError> {
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS command_attempt_batches(\
            token TEXT PRIMARY KEY,\
            view_id TEXT NOT NULL,\
            stage_id TEXT NOT NULL,\
            command_revision TEXT NOT NULL,\
            preceding_definition_revision TEXT NOT NULL,\
            expected_count INTEGER NOT NULL CHECK(expected_count BETWEEN 1 AND 1024),\
            completed INTEGER NOT NULL DEFAULT 0 CHECK(completed IN(0,1))\
         );\
         CREATE TABLE IF NOT EXISTS command_attempts(\
            view_id TEXT NOT NULL,\
            stage_id TEXT NOT NULL,\
            command_revision TEXT NOT NULL,\
            preceding_definition_revision TEXT NOT NULL,\
            source_id TEXT NOT NULL,\
            sequence TEXT NOT NULL,\
            batch_token TEXT NOT NULL,\
            state TEXT NOT NULL CHECK(state IN('reserved','ready','failed')),\
            fields_json BLOB,\
            diagnostic TEXT,\
            PRIMARY KEY(view_id,stage_id,command_revision,preceding_definition_revision,source_id,sequence),\
            FOREIGN KEY(batch_token) REFERENCES command_attempt_batches(token),\
            CHECK((state='reserved' AND fields_json IS NULL AND diagnostic IS NULL)\
               OR (state='ready' AND fields_json IS NOT NULL)\
               OR (state='failed' AND fields_json IS NULL AND diagnostic IS NOT NULL))\
         );\
         CREATE INDEX IF NOT EXISTS command_attempt_batch_idx ON command_attempts(batch_token,state);\
         PRAGMA user_version=4;",
    )?;
    tx.commit()?;
    Ok(())
}

struct EncodedAttemptOutcome {
    id: RecordId,
    state: &'static str,
    fields: Option<Vec<u8>>,
    diagnostic: Option<String>,
    payload_bytes: usize,
}

fn validate_attempt_scope(scope: &CommandAttemptScope) -> Result<(), MemoryError> {
    for (label, value) in [
        ("stage ID", scope.stage_id.as_str()),
        ("command revision", scope.command_revision.as_str()),
        (
            "preceding definition revision",
            scope.preceding_definition_revision.as_str(),
        ),
    ] {
        if value.is_empty()
            || value.len() > MAX_COMMAND_SCOPE_COMPONENT_BYTES
            || value.chars().any(char::is_control)
        {
            return Err(MemoryError::InvalidAttemptBatch(format!(
                "{label} must be 1..={MAX_COMMAND_SCOPE_COMPONENT_BYTES} bytes without controls"
            )));
        }
    }
    Ok(())
}

fn validate_attempt_ids(record_ids: &[RecordId]) -> Result<Vec<RecordId>, MemoryError> {
    if record_ids.is_empty() || record_ids.len() > MAX_COMMAND_ATTEMPT_BATCH {
        return Err(MemoryError::InvalidAttemptBatch(format!(
            "record count must be 1..={MAX_COMMAND_ATTEMPT_BATCH}"
        )));
    }
    let mut unique = BTreeSet::new();
    if record_ids.iter().any(|id| !unique.insert(*id)) {
        return Err(MemoryError::InvalidAttemptBatch(
            "duplicate record identity".into(),
        ));
    }
    Ok(record_ids.to_vec())
}

fn encode_attempt_outcome(
    id: RecordId,
    outcome: &CommandAttemptOutcome,
) -> Result<EncodedAttemptOutcome, MemoryError> {
    match outcome {
        CommandAttemptOutcome::Ready { fields, diagnostic } => {
            if fields.len() > MAX_COMMAND_ATTEMPT_FIELDS
                || fields.keys().any(|name| {
                    name.is_empty()
                        || name.len() > MAX_COMMAND_ATTEMPT_FIELD_BYTES
                        || name.chars().any(char::is_control)
                        || name == "raw"
                        || name.starts_with("_lvu_")
                })
            {
                return Err(MemoryError::InvalidAttemptBatch(format!(
                    "ready fields exceed {MAX_COMMAND_ATTEMPT_FIELDS} entries, use invalid names, or target protected/raw data"
                )));
            }
            let bytes = serde_json::to_vec(fields).map_err(invalid)?;
            if bytes.len() > MAX_COMMAND_ATTEMPT_RESULT_BYTES {
                return Err(MemoryError::InvalidAttemptBatch(format!(
                    "ready result exceeds {MAX_COMMAND_ATTEMPT_RESULT_BYTES} bytes"
                )));
            }
            if diagnostic
                .as_ref()
                .is_some_and(|value| value.len() > MAX_COMMAND_ATTEMPT_DIAGNOSTIC_BYTES)
            {
                return Err(MemoryError::InvalidAttemptBatch(format!(
                    "ready diagnostic exceeds {MAX_COMMAND_ATTEMPT_DIAGNOSTIC_BYTES} bytes"
                )));
            }
            Ok(EncodedAttemptOutcome {
                id,
                state: "ready",
                payload_bytes: bytes.len() + diagnostic.as_ref().map_or(0, String::len),
                fields: Some(bytes),
                diagnostic: diagnostic.clone(),
            })
        }
        CommandAttemptOutcome::Failed { diagnostic } => {
            if diagnostic.is_empty() || diagnostic.len() > MAX_COMMAND_ATTEMPT_DIAGNOSTIC_BYTES {
                return Err(MemoryError::InvalidAttemptBatch(format!(
                    "diagnostic must be 1..={MAX_COMMAND_ATTEMPT_DIAGNOSTIC_BYTES} bytes"
                )));
            }
            Ok(EncodedAttemptOutcome {
                id,
                state: "failed",
                payload_bytes: diagnostic.len(),
                fields: None,
                diagnostic: Some(diagnostic.clone()),
            })
        }
    }
}

fn decode_attempt_state(
    stored: Option<(String, Option<Vec<u8>>, Option<String>)>,
) -> Result<StoredCommandAttempt, MemoryError> {
    match stored {
        None => Ok(StoredCommandAttempt::NeverAttempted),
        Some((state, None, None)) if state == "reserved" => Ok(StoredCommandAttempt::Reserved),
        Some((state, Some(bytes), diagnostic)) if state == "ready" => {
            if bytes.len() > MAX_COMMAND_ATTEMPT_RESULT_BYTES
                || diagnostic
                    .as_ref()
                    .is_some_and(|value| value.len() > MAX_COMMAND_ATTEMPT_DIAGNOSTIC_BYTES)
            {
                return Err(MemoryError::InvalidData(
                    "stored command result exceeds bounds".into(),
                ));
            }
            let fields = serde_json::from_slice(&bytes).map_err(invalid)?;
            Ok(StoredCommandAttempt::Ready { fields, diagnostic })
        }
        Some((state, None, Some(diagnostic))) if state == "failed" => {
            if diagnostic.is_empty() || diagnostic.len() > MAX_COMMAND_ATTEMPT_DIAGNOSTIC_BYTES {
                return Err(MemoryError::InvalidData(
                    "stored command diagnostic exceeds bounds".into(),
                ));
            }
            Ok(StoredCommandAttempt::Failed { diagnostic })
        }
        Some(_) => Err(MemoryError::InvalidData(
            "stored command attempt has an invalid state payload".into(),
        )),
    }
}
fn import_tx(
    tx: &Transaction<'_>,
    recipe: &RecipeFile,
    _file_hash: &str,
) -> Result<(), MemoryError> {
    let document = toml::to_string_pretty(recipe)
        .map_err(|e| MemoryError::InvalidData(e.to_string()))?
        .into_bytes();
    let hash = crate::content_hash(&document);
    preflight_revision(tx, recipe)?;
    // Embedded source data is a portable snapshot. It may establish a missing
    // source identity, but cannot overwrite newer discovery/acquisition state.
    tx.execute("INSERT OR IGNORE INTO sources(source_id,definition_json,last_seen,missing) VALUES(?1,?2,0,0)",params![recipe.source.id.0.to_string(),serde_json::to_vec(&recipe.source).map_err(invalid)?])?;
    tx.execute("INSERT OR IGNORE INTO recipe_revisions(revision_id,recipe_id,name,content_hash,document) VALUES(?1,?2,?3,?4,?5)",params![recipe.revision_id.to_string(),recipe.recipe_id.0.to_string(),recipe.name,hash,document])?;
    tx.execute("INSERT INTO recipes(recipe_id,current_revision_id,name,source_id) VALUES(?1,?2,?3,?4) ON CONFLICT(recipe_id) DO UPDATE SET current_revision_id=excluded.current_revision_id,name=excluded.name",params![recipe.recipe_id.0.to_string(),recipe.revision_id.to_string(),recipe.name,recipe.source.id.0.to_string()])?;
    tx.execute("INSERT OR IGNORE INTO source_recipe_usage(source_id,recipe_id,use_count,last_used) VALUES(?1,?2,0,0)",params![recipe.source.id.0.to_string(),recipe.recipe_id.0.to_string()])?;
    Ok(())
}
fn preflight_revision(tx: &Transaction<'_>, recipe: &RecipeFile) -> Result<(), MemoryError> {
    let document = toml::to_string_pretty(recipe)
        .map_err(|e| MemoryError::InvalidData(e.to_string()))?
        .into_bytes();
    let hash = crate::content_hash(&document);
    let old: Option<(String, Vec<u8>, String)> = tx
        .query_row(
            "SELECT content_hash,document,recipe_id FROM recipe_revisions WHERE revision_id=?1",
            [recipe.revision_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    if old.as_ref().is_some_and(|value| {
        value.0 != hash || value.1 != document || value.2 != recipe.recipe_id.0.to_string()
    }) {
        return Err(MemoryError::Conflict);
    }
    Ok(())
}
fn check_limit(limit: u32) -> Result<(), MemoryError> {
    if limit == 0 || limit > MAX_PAGE {
        Err(MemoryError::InvalidLimit)
    } else {
        Ok(())
    }
}
fn validate_working_view(view: &WorkingView) -> Result<(), MemoryError> {
    let mut sources = std::collections::HashSet::new();
    if !view.presentation.source_ids.is_empty()
        && (view.presentation.source_ids.len() > 32
            || !view.presentation.source_ids.contains(&view.source_id)
            || view
                .presentation
                .source_ids
                .iter()
                .any(|id| !sources.insert(*id)))
    {
        return Err(MemoryError::InvalidData(
            "invalid ordered view sources".into(),
        ));
    }
    let mut bookmark_ids = std::collections::HashSet::new();
    if view.presentation.bookmarks.len() > 128
        || view.presentation.bookmarks.iter().any(|bookmark| {
            (bookmark.record.source_id != view.source_id
                && !view
                    .presentation
                    .source_ids
                    .contains(&bookmark.record.source_id))
                || bookmark.note.len() > 1024
                || bookmark.note.chars().any(char::is_control)
                || !bookmark_ids.insert(bookmark.record)
        })
    {
        return Err(MemoryError::InvalidData(
            "invalid bookmarks: source, duplicate identity, count or note size".into(),
        ));
    }

    if view.name.trim().is_empty() || view.name.len() > 1024 {
        return Err(MemoryError::InvalidData("invalid view name".into()));
    }
    if view.applied_search.len() > MAX_SEARCH_BYTES
        || view
            .search_draft
            .as_ref()
            .is_some_and(|value| value.len() > MAX_SEARCH_BYTES)
    {
        return Err(MemoryError::InvalidData(
            "search text exceeds 16 KiB".into(),
        ));
    }
    if let Some(expression) = &view.applied_advanced_filter
        && (expression.trim().is_empty() || expression.len() > MAX_EDITOR_BYTES)
    {
        return Err(MemoryError::InvalidData(
            "invalid applied advanced filter".into(),
        ));
    }
    if let Some(draft) = &view.advanced_filter_draft {
        let diagnostics_bytes = draft
            .diagnostics
            .iter()
            .try_fold(0usize, |total, value| total.checked_add(value.len()))
            .ok_or_else(|| MemoryError::InvalidData("draft diagnostics are too large".into()))?;
        if draft.text.len() > MAX_EDITOR_BYTES
            || draft.diagnostics.len() > MAX_DIAGNOSTICS
            || diagnostics_bytes > MAX_EDITOR_BYTES
        {
            return Err(MemoryError::InvalidData(
                "draft or diagnostics exceed bounds".into(),
            ));
        }
    }
    if view.presentation.pinned_columns.len() > 8
        || view
            .presentation
            .pinned_columns
            .iter()
            .any(|field| field.is_empty() || field.len() > 64)
        || view
            .presentation
            .color_field
            .as_ref()
            .is_some_and(|field| field.is_empty() || field.len() > 64)
    {
        return Err(MemoryError::InvalidData(
            "invalid presentation fields".into(),
        ));
    }
    if view
        .presentation
        .applied_enrichment
        .as_ref()
        .is_some_and(|value| value.len() > MAX_EDITOR_BYTES)
        || view
            .presentation
            .enrichment_draft
            .as_ref()
            .is_some_and(|draft| draft.text.len() > MAX_EDITOR_BYTES)
    {
        return Err(MemoryError::InvalidData("enrichment exceeds bounds".into()));
    }
    if let Some(chain) = &view.presentation.enrichment_chain {
        let mut ids = std::collections::HashSet::new();
        if chain.len() > 32
            || chain.iter().any(|stage| {
                stage.id.is_empty()
                    || stage.id.len() > 128
                    || stage.source.trim().is_empty()
                    || stage.source.len() > 16 * 1024
                    || stage.id.chars().any(char::is_control)
                    || !ids.insert(stage.id.as_str())
            })
        {
            return Err(MemoryError::InvalidData("invalid enrichment chain".into()));
        }
    }
    for target in [
        &view.presentation.enrichment_editing,
        &view.presentation.enrichment_selected,
    ]
    .into_iter()
    .flatten()
    {
        if target.is_empty() || target.len() > 128 {
            return Err(MemoryError::InvalidData(
                "invalid enrichment editor target".into(),
            ));
        }
    }
    Ok(())
}
fn to_i64(value: u64) -> Result<i64, MemoryError> {
    i64::try_from(value)
        .map_err(|_| MemoryError::InvalidData("integer exceeds SQLite range".into()))
}
fn invalid(e: serde_json::Error) -> MemoryError {
    MemoryError::InvalidData(e.to_string())
}
fn sql_invalid(e: serde_json::Error) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Blob, Box::new(e))
}
fn parse_uuid(s: String) -> Result<Uuid, rusqlite::Error> {
    Uuid::parse_str(&s).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}
fn json_opt<T: Serialize>(v: &Option<T>) -> Result<Option<Vec<u8>>, MemoryError> {
    v.as_ref()
        .map(|v| serde_json::to_vec(v).map_err(invalid))
        .transpose()
}
fn from_json_opt<T: for<'a> Deserialize<'a>>(
    v: Option<Vec<u8>>,
) -> Result<Option<T>, rusqlite::Error> {
    v.map(|v| serde_json::from_slice(&v).map_err(sql_invalid))
        .transpose()
}
fn source_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<SourceMetadata> {
    Ok(SourceMetadata {
        definition: serde_json::from_slice(&r.get::<_, Vec<u8>>(0)?).map_err(sql_invalid)?,
        project: r.get(1)?,
        command: r.get(2)?,
        fields: serde_json::from_slice(&r.get::<_, Vec<u8>>(3)?).map_err(sql_invalid)?,
        last_seen: r.get(4)?,
        missing: r.get(5)?,
    })
}

fn decode_recipe_revision(
    bytes: &[u8],
    id: RecipeId,
    revision: Uuid,
) -> Result<RecipeFile, MemoryError> {
    if bytes.len() as u64 > crate::MAX_DEFINITION_BYTES {
        return Err(RecipeError::TooLarge.into());
    }
    let text = std::str::from_utf8(bytes).map_err(|e| RecipeError::Toml(e.to_string()))?;
    let recipe: RecipeFile = toml::from_str(text).map_err(|e| RecipeError::Toml(e.to_string()))?;
    recipe.validate()?;
    if recipe.recipe_id != id || recipe.revision_id != revision {
        return Err(MemoryError::Conflict);
    }
    Ok(recipe)
}
