use crate::recipe::{RecipeLock, save_recipe_locked};
use crate::{MAX_SEARCH_BYTES, RecipeError, RecipeFile, SavedRecipe, read_recipe, validate_source};
use lvu_core::{RecipeId, RecordId, SourceDefinition, SourceId, ViewId};
use rusqlite::{
    Connection, OptionalExtension, Transaction, TransactionBehavior, limits::Limit, params,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};
use uuid::Uuid;

const DB_SCHEMA_VERSION: i64 = 1;
const MAX_PAGE: u32 = 100;
const MAX_RECONCILE_FILES: usize = 1024;
const MAX_SQLITE_VALUE_BYTES: i32 = 1_200_000;
const MAX_CANDIDATE_SCAN: i64 = 128;
const MAX_EDITOR_BYTES: usize = 256 * 1024;
const MAX_DIAGNOSTICS: usize = 128;

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

pub struct WorkspaceStore {
    conn: Connection,
    root: PathBuf,
}

impl WorkspaceStore {
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
        self.conn.execute("INSERT INTO sources(source_id,definition_json,project,command,fields_json,last_seen,missing) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(source_id) DO UPDATE SET definition_json=excluded.definition_json,project=excluded.project,command=excluded.command,fields_json=excluded.fields_json,last_seen=excluded.last_seen,missing=excluded.missing", params![source.definition.id.0.to_string(), serde_json::to_vec(&source.definition).map_err(invalid)?, source.project, source.command, serde_json::to_vec(&source.fields).map_err(invalid)?, source.last_seen, source.missing])?;
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
        self.conn.execute("INSERT INTO working_views(view_id,source_id,name,applied_revision_id,applied_search,search_draft,applied_advanced_filter,advanced_filter_draft_json,navigation_json,version) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)", params![view.id.0.to_string(), view.source_id.0.to_string(), view.name, view.applied_revision_id.map(|v|v.to_string()), view.applied_search, view.search_draft, view.applied_advanced_filter, json_opt(&view.advanced_filter_draft)?, serde_json::to_vec(&view.navigation).map_err(invalid)?, to_i64(view.version)?])?;
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
        let changed = self.conn.execute("UPDATE working_views SET name=?2,applied_revision_id=?3,applied_search=?4,search_draft=?5,applied_advanced_filter=?6,advanced_filter_draft_json=?7,navigation_json=?8,version=?9 WHERE view_id=?1 AND version=?10", params![view.id.0.to_string(), view.name, view.applied_revision_id.map(|v|v.to_string()), view.applied_search, view.search_draft, view.applied_advanced_filter, json_opt(&view.advanced_filter_draft)?, serde_json::to_vec(&view.navigation).map_err(invalid)?, to_i64(next)?, to_i64(expected_version)?])?;
        if changed != 1 {
            return Err(MemoryError::Conflict);
        }
        Ok(next)
    }

    pub fn get_view(&self, id: ViewId) -> Result<Option<WorkingView>, MemoryError> {
        self.conn.query_row("SELECT source_id,name,applied_revision_id,applied_search,search_draft,applied_advanced_filter,advanced_filter_draft_json,navigation_json,version FROM working_views WHERE view_id=?1", [id.0.to_string()], |r| {
            let version: i64 = r.get(8)?;
            Ok(WorkingView { id, source_id: SourceId(parse_uuid(r.get::<_,String>(0)?)?), name:r.get(1)?, applied_revision_id:r.get::<_,Option<String>>(2)?.map(parse_uuid).transpose()?, applied_search:r.get(3)?, search_draft:r.get(4)?, applied_advanced_filter:r.get(5)?, advanced_filter_draft:from_json_opt(r.get(6)?)?, navigation: serde_json::from_slice(&r.get::<_,Vec<u8>>(7)?).map_err(sql_invalid)?, version:u64::try_from(version).map_err(|e|rusqlite::Error::FromSqlConversionFailure(8,rusqlite::types::Type::Integer,Box::new(e)))? })
        }).optional().map_err(MemoryError::from)
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
        let mut stmt=self.conn.prepare("SELECT r.recipe_id,r.current_revision_id,r.name,s.project,s.command,s.fields_json,COALESCE(u.use_count,0),COALESCE(u.last_used,0),COALESCE((SELECT SUM(CASE outcome WHEN 'accepted' THEN 1 ELSE -1 END) FROM suggestion_outcomes o WHERE o.source_id=?1 AND o.recipe_id=r.recipe_id),0) FROM recipes r JOIN sources s ON s.source_id=r.source_id LEFT JOIN source_recipe_usage u ON u.source_id=?1 AND u.recipe_id=r.recipe_id ORDER BY (s.project=?2) DESC,(s.command=?3) DESC,COALESCE(u.use_count,0) DESC,r.recipe_id LIMIT ?4")?;
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
                    r.get::<_, i64>(6)?,
                    r.get::<_, i64>(7)?,
                    r.get::<_, i64>(8)?,
                ))
            },
        )?;
        for row in rows {
            let (rid, rev, name, p, c, fjson, uses, last, outcome) = row?;
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
            let exact = fields
                .iter()
                .filter(|(k, v)| candidate_fields.get(*k) == Some(*v))
                .count() as i64;
            let names = fields
                .keys()
                .filter(|k| candidate_fields.contains_key(*k))
                .count() as i64;
            score += exact * 10 + (names - exact) * 3;
            if exact > 0 {
                evidence.push(format!("{exact} matching field types"));
            } else if names > 0 {
                evidence.push(format!("{names} matching field names"));
            }
            if uses > 0 {
                evidence.push(format!("used {uses} times on this source"));
            }
            if last > 0 {
                evidence.push(format!("last used {last}"));
            }
            candidates.push(RecipeCandidate {
                recipe_id: RecipeId(parse_uuid(rid)?),
                revision_id: parse_uuid(rev)?,
                name,
                score,
                evidence,
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
}

fn migrate_v1(conn: &Connection) -> Result<(), MemoryError> {
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch("CREATE TABLE sources(source_id TEXT PRIMARY KEY,definition_json BLOB NOT NULL,project TEXT,command TEXT,fields_json BLOB NOT NULL DEFAULT '{}',last_seen INTEGER NOT NULL DEFAULT 0,missing INTEGER NOT NULL DEFAULT 0); CREATE TABLE source_fingerprints(source_id TEXT NOT NULL,kind TEXT NOT NULL,value TEXT NOT NULL,PRIMARY KEY(source_id,kind,value),FOREIGN KEY(source_id) REFERENCES sources(source_id)); CREATE TABLE recipe_revisions(revision_id TEXT PRIMARY KEY,recipe_id TEXT NOT NULL,name TEXT NOT NULL,content_hash TEXT NOT NULL,document BLOB NOT NULL); CREATE TABLE recipes(recipe_id TEXT PRIMARY KEY,current_revision_id TEXT NOT NULL,name TEXT NOT NULL,source_id TEXT NOT NULL,FOREIGN KEY(current_revision_id) REFERENCES recipe_revisions(revision_id)); CREATE TABLE source_recipe_usage(source_id TEXT NOT NULL,recipe_id TEXT NOT NULL,use_count INTEGER NOT NULL,last_used INTEGER NOT NULL,PRIMARY KEY(source_id,recipe_id),FOREIGN KEY(source_id) REFERENCES sources(source_id),FOREIGN KEY(recipe_id) REFERENCES recipes(recipe_id)); CREATE TABLE suggestion_outcomes(id INTEGER PRIMARY KEY,source_id TEXT NOT NULL,recipe_id TEXT NOT NULL,revision_id TEXT NOT NULL,outcome TEXT NOT NULL CHECK(outcome IN('accepted','rejected')),recorded_at INTEGER NOT NULL); CREATE TABLE working_views(view_id TEXT PRIMARY KEY,source_id TEXT NOT NULL,name TEXT NOT NULL,applied_revision_id TEXT,applied_search TEXT NOT NULL,search_draft TEXT,applied_advanced_filter TEXT,advanced_filter_draft_json BLOB,navigation_json BLOB NOT NULL,version INTEGER NOT NULL,FOREIGN KEY(source_id) REFERENCES sources(source_id)); CREATE INDEX sources_recent_idx ON sources(last_seen DESC,source_id DESC); CREATE INDEX fingerprints_lookup_idx ON source_fingerprints(kind,value,source_id); CREATE INDEX suggestion_lookup_idx ON suggestion_outcomes(source_id,recipe_id,outcome); CREATE INDEX recipe_history_idx ON recipe_revisions(recipe_id); PRAGMA user_version=1;")?;
    tx.commit()?;
    Ok(())
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
