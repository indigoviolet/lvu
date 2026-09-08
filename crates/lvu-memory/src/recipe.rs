use lvu_core::{RecipeId, SourceDefinition, SourceId, ViewId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};
use uuid::Uuid;

pub const RECIPE_SCHEMA_VERSION: u32 = 1;
/// Shared immutable revision documents live beside the canonical current
/// pointers so every isolated catalog reconciles the same publication history.
pub(crate) const SHARED_REVISIONS_DIR: &str = ".revisions";
pub(crate) const MAX_SHARED_REVISIONS_PER_RECIPE: usize = 256;
pub(crate) const MAX_SHARED_REVISION_BYTES: u64 = 32 * 1024 * 1024;
pub const MAX_DEFINITION_BYTES: u64 = 1024 * 1024;
pub const MAX_SEARCH_BYTES: usize = 16 * 1024;
pub const PROTECTED_COLUMNS: &[&str] = &[
    "raw",
    "_lvu_source_id",
    "_lvu_sequence",
    "_lvu_captured_at",
    "_lvu_stream",
];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RecipeFile {
    pub schema_version: u32,
    pub recipe_id: RecipeId,
    pub revision_id: Uuid,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// When this revision was saved, in Unix nanoseconds. Additive and
    /// defaulted, so a recipe written before the field existed reads as `None`
    /// and the UI shows it as unknown rather than inventing a date; the schema
    /// version does not move and no stored file is rewritten.
    ///
    /// It belongs to the *revision*, not to the recipe: every revision document
    /// carries the moment it was saved, which is what History dates.
    #[serde(default)]
    pub saved_at_unix_nanos: Option<i64>,
    pub source: SourceDefinition,
    pub view: NamedViewDefinition,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NamedViewDefinition {
    pub schema_version: u32,
    pub id: ViewId,
    pub name: String,
    pub source_ids: Vec<SourceId>,
    #[serde(default)]
    pub stages: Vec<StageDefinition>,
    #[serde(default)]
    pub search: String,
    pub advanced_filter: Option<ExpressionDefinition>,
    #[serde(default)]
    pub pinned_columns: Vec<String>,
    #[serde(default)]
    pub color_rules: Vec<ColorRule>,
    #[serde(default)]
    pub time_policy: TimePolicy,
    #[serde(default)]
    pub time_basis: TimeBasis,
    #[serde(default)]
    pub grouping: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StageDefinition {
    /// Editable native extraction source, either named-capture regex shorthand
    /// or a named Polars expression. Interpretation happens on explicit apply.
    Extraction { id: String, source: String },
    Polars {
        id: Uuid,
        expression: String,
        output: String,
    },
    Command {
        id: Uuid,
        command: lvu_core::CommandDefinition,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExpressionDefinition {
    pub expression: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ColorRule {
    pub expression: String,
    pub style: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TimePolicy {
    #[default]
    All,
    Absolute {
        start_unix_nanos: i64,
        end_unix_nanos: i64,
    },
    Recent {
        seconds: u64,
    },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeBasis {
    #[default]
    Capture,
    Event,
    /// UTC RFC3339 strings from the accepted timestamp_utc enrichment.
    Extracted,
    /// A field the user declared explicitly. The declaration itself is stored
    /// beside the basis as a `TimeFieldSelection` token.
    Selected,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedRecipe {
    pub path: PathBuf,
    pub content_hash: String,
    pub revision_id: Uuid,
}

#[derive(Debug, thiserror::Error)]
pub enum RecipeError {
    #[error("definition is larger than {MAX_DEFINITION_BYTES} bytes")]
    TooLarge,
    #[error("unsupported recipe schema version {0}")]
    FutureVersion(u32),
    #[error("invalid definition: {0}")]
    Invalid(String),
    #[error("recipe file changed externally")]
    Conflict,
    #[error("recipe path escapes the application recipe directory")]
    UnsafePath,
    #[error("recipe path is not a regular file")]
    NotRegularFile,
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("malformed TOML: {0}")]
    Toml(String),
}

impl RecipeFile {
    pub fn validate(&self) -> Result<(), RecipeError> {
        if self.schema_version != RECIPE_SCHEMA_VERSION {
            return Err(RecipeError::FutureVersion(self.schema_version));
        }
        if self.view.schema_version != 1 {
            return Err(RecipeError::Invalid("view schema_version must be 1".into()));
        }
        validate_source(&self.source)?;
        if self.name.trim().is_empty()
            || self.view.name.trim().is_empty()
            || self.source.name.trim().is_empty()
        {
            return Err(RecipeError::Invalid("names must not be empty".into()));
        }
        if !self.view.source_ids.contains(&self.source.id) {
            return Err(RecipeError::Invalid(
                "view must reference its source".into(),
            ));
        }
        let validate_expr = |value: &str| {
            if value.trim().is_empty() {
                Err(RecipeError::Invalid("expressions must not be empty".into()))
            } else {
                Ok(())
            }
        };
        if self.view.search.len() > MAX_SEARCH_BYTES {
            return Err(RecipeError::Invalid("search text is too large".into()));
        }
        if let Some(filter) = &self.view.advanced_filter {
            validate_expr(&filter.expression)?;
        }
        if self.view.stages.len() > 32 {
            return Err(RecipeError::Invalid("too many enrichment stages".into()));
        }
        let mut stage_ids = std::collections::HashSet::new();
        for stage in &self.view.stages {
            let id = match stage {
                StageDefinition::Extraction { id, .. } => id.clone(),
                StageDefinition::Polars { id, .. } | StageDefinition::Command { id, .. } => {
                    id.to_string()
                }
            };
            if id.is_empty() || id.len() > 128 || !stage_ids.insert(id) {
                return Err(RecipeError::Invalid(
                    "invalid or duplicate stage identity".into(),
                ));
            }
            match stage {
                StageDefinition::Extraction { source, .. } => {
                    validate_expr(source)?;
                    if source.len() > MAX_SEARCH_BYTES {
                        return Err(RecipeError::Invalid(
                            "enrichment source is too large".into(),
                        ));
                    }
                }
                StageDefinition::Polars {
                    expression, output, ..
                } => {
                    validate_expr(expression)?;
                    validate_output(output)?;
                }
                StageDefinition::Command { command, .. } => match &command.program {
                    lvu_core::CommandProgram::Shell { text } if text.trim().is_empty() => {
                        return Err(RecipeError::Invalid(
                            "shell command must not be empty".into(),
                        ));
                    }
                    lvu_core::CommandProgram::Exec { executable, .. }
                        if executable.as_os_str().is_empty() =>
                    {
                        return Err(RecipeError::Invalid(
                            "command executable must not be empty".into(),
                        ));
                    }
                    _ => {}
                },
            }
        }
        for column in &self.view.pinned_columns {
            validate_column(column)?;
        }
        for rule in &self.view.color_rules {
            validate_expr(&rule.expression)?;
            if rule.style.trim().is_empty() {
                return Err(RecipeError::Invalid("color style must not be empty".into()));
            }
        }
        if let TimePolicy::Absolute {
            start_unix_nanos,
            end_unix_nanos,
        } = self.view.time_policy
            && start_unix_nanos >= end_unix_nanos
        {
            return Err(RecipeError::Invalid(
                "time range starts after it ends".into(),
            ));
        }
        if matches!(self.view.time_policy, TimePolicy::Recent { seconds: 0 })
            || matches!(
                self.view.time_policy,
                TimePolicy::Recent { seconds }
                    if seconds > i64::MAX as u64 / 1_000_000_000
            )
        {
            return Err(RecipeError::Invalid(
                "recent time range duration is outside the supported range".into(),
            ));
        }
        Ok(())
    }
}

pub fn validate_source(source: &SourceDefinition) -> Result<(), RecipeError> {
    if source.schema_version != 1 {
        return Err(RecipeError::FutureVersion(source.schema_version));
    }
    if source.name.trim().is_empty() {
        return Err(RecipeError::Invalid("source name must not be empty".into()));
    }
    match &source.acquisition {
        lvu_core::Acquisition::File { path, .. } if path.as_os_str().is_empty() => {
            Err(RecipeError::Invalid("source path must not be empty".into()))
        }
        lvu_core::Acquisition::Command { command } => match &command.program {
            lvu_core::CommandProgram::Shell { text } if text.trim().is_empty() => Err(
                RecipeError::Invalid("shell command must not be empty".into()),
            ),
            lvu_core::CommandProgram::Exec { executable, .. }
                if executable.as_os_str().is_empty() =>
            {
                Err(RecipeError::Invalid(
                    "command executable must not be empty".into(),
                ))
            }
            _ => Ok(()),
        },
        lvu_core::Acquisition::Http { url, .. } if url.trim().is_empty() => {
            Err(RecipeError::Invalid("source URL must not be empty".into()))
        }
        _ => Ok(()),
    }
}

pub fn validate_output(name: &str) -> Result<(), RecipeError> {
    validate_column(name)?;
    if PROTECTED_COLUMNS.contains(&name) || name.starts_with("_lvu_") {
        return Err(RecipeError::Invalid(format!(
            "protected output column {name}"
        )));
    }
    Ok(())
}
fn validate_column(name: &str) -> Result<(), RecipeError> {
    if name.is_empty() || name.len() > 256 || name.chars().any(char::is_control) {
        return Err(RecipeError::Invalid("invalid column name".into()));
    }
    Ok(())
}

pub fn recipe_path(root: &Path, id: RecipeId) -> Result<PathBuf, RecipeError> {
    recipe_path_in(&root.join("recipes"), id)
}

pub(crate) fn recipe_path_in(recipes_root: &Path, id: RecipeId) -> Result<PathBuf, RecipeError> {
    let path = recipes_root.join(format!("{}.toml", id.0));
    if path.parent() != Some(recipes_root) {
        return Err(RecipeError::UnsafePath);
    }
    Ok(path)
}

pub(crate) fn shared_revisions_root(recipes_root: &Path) -> PathBuf {
    recipes_root.join(SHARED_REVISIONS_DIR)
}

pub(crate) fn shared_revision_dir(
    recipes_root: &Path,
    id: RecipeId,
) -> Result<PathBuf, RecipeError> {
    let root = shared_revisions_root(recipes_root);
    let dir = root.join(id.0.to_string());
    if dir.parent() != Some(root.as_path()) {
        return Err(RecipeError::UnsafePath);
    }
    Ok(dir)
}

/// Publishes the exact validated bytes of one revision under a name that is
/// never replaced. The bytes are written to an owned temporary file and synced
/// first, so an interrupted publication can only leave a private temporary
/// file behind, never a partial document occupying the final name.
pub(crate) fn publish_shared_revision(
    recipes_root: &Path,
    recipe: &RecipeFile,
    bytes: &[u8],
) -> Result<(), RecipeError> {
    let dir = shared_revision_dir(recipes_root, recipe.recipe_id)?;
    let final_path = dir.join(format!("{}.toml", recipe.revision_id));
    if final_path.parent() != Some(dir.as_path()) {
        return Err(RecipeError::UnsafePath);
    }
    fs::create_dir_all(&dir).map_err(|source| RecipeError::Io {
        path: dir.clone(),
        source,
    })?;
    if let Some(existing) = read_published_revision(&final_path)? {
        return if existing == bytes {
            Ok(())
        } else {
            Err(RecipeError::Conflict)
        };
    }
    let temporary = dir.join(format!(".{}.{}.tmp", recipe.revision_id, Uuid::new_v4()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    let mut file = options.open(&temporary).map_err(|source| RecipeError::Io {
        path: temporary.clone(),
        source,
    })?;
    let result = (|| {
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|source| RecipeError::Io {
                path: temporary.clone(),
                source,
            })?;
        // Linking publishes the finished bytes atomically and never replaces an
        // existing winner. A concurrent publisher of the same revision loses the
        // link and is accepted only when its bytes are identical.
        match fs::hard_link(&temporary, &final_path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                match read_published_revision(&final_path)? {
                    Some(existing) if existing == bytes => Ok(()),
                    _ => Err(RecipeError::Conflict),
                }
            }
            Err(source) => Err(RecipeError::Io {
                path: final_path.clone(),
                source,
            }),
        }?;
        fs::File::open(&dir)
            .and_then(|handle| handle.sync_all())
            .map_err(|source| RecipeError::Io {
                path: dir.clone(),
                source,
            })
    })();
    drop(file);
    let _ = fs::remove_file(&temporary);
    result
}

fn read_published_revision(path: &Path) -> Result<Option<Vec<u8>>, RecipeError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(Some(read_bounded_regular(path)?)),
        Ok(_) => Err(RecipeError::UnsafePath),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(RecipeError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

pub fn read_recipe(path: &Path) -> Result<(RecipeFile, String), RecipeError> {
    let bytes = read_bounded_regular(path)?;
    let text = std::str::from_utf8(&bytes).map_err(|e| RecipeError::Toml(e.to_string()))?;
    let recipe: RecipeFile = toml::from_str(text).map_err(|e| RecipeError::Toml(e.to_string()))?;
    recipe.validate()?;
    Ok((recipe, content_hash(&bytes)))
}

pub fn save_recipe(
    root: &Path,
    recipe: &RecipeFile,
    expected_hash: Option<&str>,
) -> Result<SavedRecipe, RecipeError> {
    recipe.validate()?;
    let guard = RecipeLock::acquire(root, recipe.recipe_id)?;
    save_recipe_locked(&guard, recipe, expected_hash)
}

/// Export one immutable definition to a new user-selected path. Existing files
/// (including symlinks) are never replaced. Publication happens only after sync.
pub fn export_recipe(path: &Path, recipe: &RecipeFile) -> Result<SavedRecipe, RecipeError> {
    recipe.validate()?;
    let bytes = toml::to_string_pretty(recipe)
        .map_err(|error| RecipeError::Toml(error.to_string()))?
        .into_bytes();
    if bytes.len() as u64 > MAX_DEFINITION_BYTES {
        return Err(RecipeError::TooLarge);
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent = parent.canonicalize().map_err(|source| RecipeError::Io {
        path: parent.into(),
        source,
    })?;
    let name = path.file_name().ok_or(RecipeError::UnsafePath)?;
    let output = parent.join(name);
    let temporary = parent.join(format!(".lvu-export-{}.tmp", Uuid::new_v4()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary).map_err(|source| RecipeError::Io {
        path: temporary.clone(),
        source,
    })?;
    let result = (|| {
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|source| RecipeError::Io {
                path: temporary.clone(),
                source,
            })?;
        // Linking is an atomic no-replace publication on the same filesystem.
        fs::hard_link(&temporary, &output).map_err(|source| RecipeError::Io {
            path: output.clone(),
            source,
        })?;
        #[cfg(unix)]
        fs::File::open(&parent)
            .and_then(|file| file.sync_all())
            .map_err(|source| RecipeError::Io {
                path: parent.clone(),
                source,
            })?;
        Ok(SavedRecipe {
            path: output,
            content_hash: content_hash(&bytes),
            revision_id: recipe.revision_id,
        })
    })();
    drop(file);
    let _ = fs::remove_file(&temporary);
    result
}

pub(crate) struct RecipeLock {
    pub(crate) path: PathBuf,
    _file: fs::File,
}

impl RecipeLock {
    pub(crate) fn acquire(root: &Path, id: RecipeId) -> Result<Self, RecipeError> {
        Self::acquire_in(&root.join("recipes"), id)
    }

    pub(crate) fn acquire_in(recipes_dir: &Path, id: RecipeId) -> Result<Self, RecipeError> {
        use fs2::FileExt;
        fs::create_dir_all(recipes_dir).map_err(|source| RecipeError::Io {
            path: recipes_dir.into(),
            source,
        })?;
        let recipe_dir_metadata =
            fs::symlink_metadata(recipes_dir).map_err(|source| RecipeError::Io {
                path: recipes_dir.to_path_buf(),
                source,
            })?;
        if recipe_dir_metadata.file_type().is_symlink() || !recipe_dir_metadata.is_dir() {
            return Err(RecipeError::UnsafePath);
        }
        let path = recipe_path_in(recipes_dir, id)?;
        // One application-owned lock serializes the cross-file TOML/SQLite
        // publication order, including startup reconciliation.
        let lock_path = recipes_dir.join(".write.lock");
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|source| RecipeError::Io {
                path: lock_path,
                source,
            })?;
        lock.try_lock_exclusive().map_err(|error| {
            if error.kind() == std::io::ErrorKind::WouldBlock {
                RecipeError::Conflict
            } else {
                RecipeError::Io {
                    path: path.clone(),
                    source: error,
                }
            }
        })?;
        Ok(Self { path, _file: lock })
    }
}

pub(crate) fn save_recipe_locked(
    guard: &RecipeLock,
    recipe: &RecipeFile,
    expected_hash: Option<&str>,
) -> Result<SavedRecipe, RecipeError> {
    recipe.validate()?;
    let path = &guard.path;
    if path.exists() {
        let (current_definition, current_hash) = read_recipe(path)?;
        if current_definition.recipe_id != recipe.recipe_id {
            return Err(RecipeError::Conflict);
        }
        if expected_hash != Some(current_hash.as_str()) {
            return Err(RecipeError::Conflict);
        }
    } else if expected_hash.is_some() {
        return Err(RecipeError::Conflict);
    }
    let bytes = toml::to_string_pretty(recipe)
        .map_err(|e| RecipeError::Toml(e.to_string()))?
        .into_bytes();
    if bytes.len() as u64 > MAX_DEFINITION_BYTES {
        return Err(RecipeError::TooLarge);
    }
    let recipes_root = path.parent().ok_or(RecipeError::UnsafePath)?;
    // Order: the immutable revision document is durable before any pointer can
    // name it, so an interrupted publication can lose the pointer but never the
    // history another catalog must reconcile.
    publish_shared_revision(recipes_root, recipe, &bytes)?;
    let temp = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        let mut file = options.open(&temp).map_err(|source| RecipeError::Io {
            path: temp.clone(),
            source,
        })?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|source| RecipeError::Io {
                path: temp.clone(),
                source,
            })?;
        fs::rename(&temp, path).map_err(|source| RecipeError::Io {
            path: path.clone(),
            source,
        })?;
        fs::File::open(path.parent().unwrap())
            .and_then(|f| f.sync_all())
            .map_err(|source| RecipeError::Io {
                path: path.parent().unwrap().into(),
                source,
            })
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result?;
    Ok(SavedRecipe {
        path: path.clone(),
        content_hash: content_hash(&bytes),
        revision_id: recipe.revision_id,
    })
}

fn read_bounded_regular(path: &Path) -> Result<Vec<u8>, RecipeError> {
    let file = fs::File::open(path).map_err(|source| RecipeError::Io {
        path: path.into(),
        source,
    })?;
    if !file
        .metadata()
        .map_err(|source| RecipeError::Io {
            path: path.into(),
            source,
        })?
        .is_file()
    {
        return Err(RecipeError::NotRegularFile);
    }
    let mut bytes = Vec::new();
    file.take(MAX_DEFINITION_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| RecipeError::Io {
            path: path.into(),
            source,
        })?;
    if bytes.len() as u64 > MAX_DEFINITION_BYTES {
        return Err(RecipeError::TooLarge);
    }
    Ok(bytes)
}

pub fn content_hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Default view constraint: literal, Unicode case-insensitive containment.
/// An empty needle is unconstrained. Query consumers combine this result with
/// an optional advanced Polars predicate using logical AND.
pub fn literal_search_matches(haystack: &str, needle: &str) -> bool {
    needle.is_empty() || haystack.to_lowercase().contains(&needle.to_lowercase())
}

pub fn environment_names(source: &SourceDefinition) -> BTreeMap<String, String> {
    match &source.acquisition {
        lvu_core::Acquisition::Command { command } => command
            .environment
            .keys()
            .map(|key| (key.clone(), "configured".into()))
            .collect(),
        _ => BTreeMap::new(),
    }
}
